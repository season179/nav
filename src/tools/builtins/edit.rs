//! `edit` — precise text replacement. Edits are resolved against the original
//! file, may not overlap, preserve the file's BOM and line-ending style, and
//! fall back to fuzzy matching for common model/text transport mismatches.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use unicode_normalization_alignments::UnicodeNormalization;

use super::support::paths::resolve_in_cwd;
use super::support::text::{
    LineEnding, bytes_preserving_file_style, detect_line_ending, normalize_line_endings_to_lf,
    strip_utf8_bom,
};
use super::support::truncate::cap_head;
use super::{CancelFlag, Tool, ToolError, ToolOutput};

pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by precise text replacement. Every edits[].oldText must \
         match a unique, non-overlapping region unless replaceAll is true. LF \
         oldText matches CRLF files, the original BOM/line endings are preserved, \
         and a fuzzy fallback handles trailing whitespace, Unicode compatibility \
         forms, and smart punctuation."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some(
            "Make precise file edits with text replacement, including multiple disjoint edits in one call",
        )
    }

    fn prompt_guidelines(&self) -> &'static [&'static str] {
        &[
            "Use edit for precise changes (edits[].oldText should be minimal but unique)",
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
            "Use edits[].replaceAll only when every occurrence of oldText should change.",
        ]
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements, each matched against the original file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text to replace; must be unique in the file" },
                            "newText": { "type": "string", "description": "Replacement text" },
                            "replaceAll": { "type": "boolean", "description": "Replace every occurrence of oldText. Default false." }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        _cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let path = arg_path(args)?;
        let resolved = resolve_in_cwd(cwd, path)?;
        let bytes = fs::read(&resolved)
            .map_err(|error| ToolError::new(format!("could not read {path}: {error}")))?;
        let (had_bom, body) = strip_utf8_bom(&bytes);
        let original = String::from_utf8(body.to_vec())
            .map_err(|error| ToolError::new(format!("{path} is not valid UTF-8: {error}")))?;
        let line_ending = detect_line_ending(&original);
        let base = normalize_line_endings_to_lf(&original);

        let edits = collect_edits(args)?;
        let mut fuzzy_base = None;
        let mut resolved_edits = Vec::with_capacity(edits.len());
        for edit in &edits {
            resolved_edits.extend(resolve_edit(path, &base, edit, &mut fuzzy_base)?);
        }

        resolved_edits.sort_by_key(|edit| edit.start);
        reject_overlapping_edits(&resolved_edits)?;

        let result = apply_resolved_edits(&base, &resolved_edits);
        if result == base {
            return Err(ToolError::new("edit produced no changes"));
        }
        write_preserving_file_style(&resolved, path, &result, had_bom, line_ending)?;

        Ok(ToolOutput::new(cap_head(&success_message(
            path,
            &base,
            edits.len(),
            &resolved_edits,
        ))))
    }
}

#[derive(Clone)]
struct EditRequest {
    old: String,
    new: String,
    replace_all: bool,
}

#[derive(Clone)]
struct ResolvedEdit {
    start: usize,
    end: usize,
    new_text: String,
    fuzzy: bool,
}

struct FuzzyMap {
    text: String,
    offsets: Vec<usize>,
}

fn arg_path(args: &Value) -> Result<&str, ToolError> {
    string_alias(args, &["path", "file_path"])
        .ok_or_else(|| ToolError::new("missing string argument path"))
}

fn collect_edits(args: &Value) -> Result<Vec<EditRequest>, ToolError> {
    let mut edits = match args.get("edits") {
        Some(raw_edits) => parse_edits_value(raw_edits)?,
        None => Vec::new(),
    };

    if let Some(top_level) = parse_top_level_edit(args)? {
        edits.push(top_level);
    }

    if edits.is_empty() {
        return Err(ToolError::new(
            "edits must be an array, or provide top-level oldText/newText",
        ));
    }

    Ok(edits)
}

fn parse_edits_value(raw_edits: &Value) -> Result<Vec<EditRequest>, ToolError> {
    match raw_edits {
        Value::Array(values) => values.iter().map(|edit| parse_edit(edit, None)).collect(),
        Value::String(text) => {
            let parsed: Value = serde_json::from_str(text)
                .map_err(|error| ToolError::new(format!("edits string is not JSON: {error}")))?;
            let values = parsed
                .as_array()
                .ok_or_else(|| ToolError::new("edits string must decode to an array"))?;
            values.iter().map(|edit| parse_edit(edit, None)).collect()
        }
        _ => Err(ToolError::new("edits must be an array")),
    }
}

fn parse_top_level_edit(args: &Value) -> Result<Option<EditRequest>, ToolError> {
    let old = string_alias(args, &["oldText", "old_string"]);
    let new = string_alias(args, &["newText", "new_string"]);
    match (old, new) {
        (Some(_), None) => Err(ToolError::new("top-level oldText needs newText")),
        (None, Some(_)) => Err(ToolError::new("top-level newText needs oldText")),
        (Some(_), Some(_)) => {
            parse_edit(args, bool_alias(args, &["replaceAll", "replace_all"])).map(Some)
        }
        (None, None) => Ok(None),
    }
}

fn parse_edit(edit: &Value, replace_all_override: Option<bool>) -> Result<EditRequest, ToolError> {
    let old = string_alias(edit, &["oldText", "old_string"])
        .ok_or_else(|| ToolError::new("each edit needs a string oldText"))?;
    let new = string_alias(edit, &["newText", "new_string"])
        .ok_or_else(|| ToolError::new("each edit needs a string newText"))?;
    if old.is_empty() {
        return Err(ToolError::new("oldText must not be empty"));
    }
    Ok(EditRequest {
        old: normalize_line_endings_to_lf(old),
        new: normalize_line_endings_to_lf(new),
        replace_all: replace_all_override
            .or_else(|| bool_alias(edit, &["replaceAll", "replace_all"]))
            .unwrap_or(false),
    })
}

fn string_alias<'a>(value: &'a Value, aliases: &[&str]) -> Option<&'a str> {
    aliases
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn bool_alias(value: &Value, aliases: &[&str]) -> Option<bool> {
    aliases
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

fn write_preserving_file_style(
    resolved: &Path,
    path: &str,
    text: &str,
    had_bom: bool,
    line_ending: LineEnding,
) -> Result<(), ToolError> {
    fs::write(
        resolved,
        bytes_preserving_file_style(text, had_bom, line_ending),
    )
    .map_err(|error| ToolError::new(format!("could not write {path}: {error}")))
}

fn resolve_edit(
    path: &str,
    base: &str,
    edit: &EditRequest,
    fuzzy_base: &mut Option<FuzzyMap>,
) -> Result<Vec<ResolvedEdit>, ToolError> {
    let exact_matches = find_matches(base, &edit.old)
        .into_iter()
        .map(|(start, end)| (start, end, false))
        .collect::<Vec<_>>();
    let matches = if exact_matches.is_empty() {
        let fuzzy = fuzzy_base.get_or_insert_with(|| normalize_for_fuzzy_match(base));
        let fuzzy_old = normalize_for_fuzzy_match(&edit.old).text;
        if fuzzy_old.is_empty() {
            return Err(ToolError::new("oldText normalizes to empty"));
        }
        find_matches(&fuzzy.text, &fuzzy_old)
            .into_iter()
            .map(|(start, end)| (fuzzy.offset(start), fuzzy.offset(end), true))
            .collect::<Vec<_>>()
    } else {
        exact_matches
    };

    match matches.len() {
        0 => Err(ToolError::new(format!(
            "oldText not found in {path}: {:?}",
            edit.old
        ))),
        1 => Ok(vec![resolved_edit(matches[0], edit)]),
        _ if edit.replace_all => Ok(matches
            .into_iter()
            .map(|matched| resolved_edit(matched, edit))
            .collect()),
        count => Err(ToolError::new(format!(
            "oldText is not unique in {path} ({count} matches); add context or set replaceAll=true: {:?}",
            edit.old
        ))),
    }
}

fn resolved_edit((start, end, fuzzy): (usize, usize, bool), edit: &EditRequest) -> ResolvedEdit {
    ResolvedEdit {
        start,
        end,
        new_text: edit.new.clone(),
        fuzzy,
    }
}

fn reject_overlapping_edits(edits: &[ResolvedEdit]) -> Result<(), ToolError> {
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ToolError::new(
                "edits overlap; merge nearby changes into one edit",
            ));
        }
    }
    Ok(())
}

fn apply_resolved_edits(base: &str, edits: &[ResolvedEdit]) -> String {
    let mut result = String::with_capacity(base.len());
    let mut cursor = 0;
    for edit in edits {
        result.push_str(&base[cursor..edit.start]);
        result.push_str(&edit.new_text);
        cursor = edit.end;
    }
    result.push_str(&base[cursor..]);
    result
}

fn find_matches(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    haystack
        .match_indices(needle)
        .map(|(index, matched)| (index, index + matched.len()))
        .collect()
}

fn normalize_for_fuzzy_match(input: &str) -> FuzzyMap {
    let mut text = String::with_capacity(input.len());
    let mut offsets = vec![0];
    let mut position = 0;

    for segment in input.split_inclusive('\n') {
        let segment_start = position;
        let segment_end = segment_start + segment.len();
        let has_newline = segment.ends_with('\n');
        let content_end = if has_newline {
            segment_end - '\n'.len_utf8()
        } else {
            segment_end
        };
        let content = &input[segment_start..content_end];
        let trimmed_len = trim_horizontal_whitespace_end(content);

        push_nfkc_fuzzy_segment(
            &mut text,
            &mut offsets,
            &content[..trimmed_len],
            segment_start,
        );

        if trimmed_len < content.len() {
            set_offset(&mut offsets, text.len(), content_end);
        }

        if has_newline {
            push_mapped_char(&mut text, &mut offsets, '\n', content_end, segment_end);
        }
        position = segment_end;
    }

    FuzzyMap { text, offsets }
}

fn trim_horizontal_whitespace_end(text: &str) -> usize {
    text.char_indices()
        .rev()
        .find(|(_, ch)| !is_horizontal_whitespace(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0)
}

fn is_horizontal_whitespace(ch: char) -> bool {
    ch != '\n' && ch != '\r' && ch.is_whitespace()
}

fn push_nfkc_fuzzy_segment(
    text: &mut String,
    offsets: &mut Vec<usize>,
    segment: &str,
    absolute_start: usize,
) {
    let mut boundaries = segment
        .char_indices()
        .map(|(index, _)| absolute_start + index)
        .collect::<Vec<_>>();
    boundaries.push(absolute_start + segment.len());

    let mut source_index = 0;
    let last_index = boundaries.len().saturating_sub(1);
    for (ch, change) in segment.nfkc() {
        // Preserve byte offsets through NFKC so fuzzy matches map back to the
        // original file slice.
        let input_start = boundaries
            .get(source_index)
            .copied()
            .unwrap_or_else(|| boundaries[last_index]);
        let consumed = (1isize - change).max(0) as usize;
        let next_index = (source_index + consumed).min(last_index);
        let input_end = boundaries[next_index];
        push_fuzzy_char(text, offsets, ch, input_start, input_end);
        source_index = next_index;
    }
}

fn push_fuzzy_char(
    text: &mut String,
    offsets: &mut Vec<usize>,
    ch: char,
    input_start: usize,
    input_end: usize,
) {
    let normalized = normalize_fuzzy_char(ch);
    push_mapped_char(text, offsets, normalized, input_start, input_end);
}

fn normalize_fuzzy_char(ch: char) -> char {
    match ch {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch),
        _ => ch,
    }
}

fn push_mapped_char(
    text: &mut String,
    offsets: &mut Vec<usize>,
    ch: char,
    input_start: usize,
    input_end: usize,
) {
    let output_start = text.len();
    text.push(ch);
    let output_end = text.len();
    if offsets.len() <= output_end {
        offsets.resize(output_end + 1, input_start);
    }
    offsets[output_start] = input_start;
    for offset in offsets.iter_mut().take(output_end).skip(output_start + 1) {
        *offset = input_start;
    }
    offsets[output_end] = input_end;
}

fn set_offset(offsets: &mut Vec<usize>, output_offset: usize, input_offset: usize) {
    if offsets.len() <= output_offset {
        offsets.resize(output_offset + 1, input_offset);
    }
    offsets[output_offset] = input_offset;
}

impl FuzzyMap {
    fn offset(&self, index: usize) -> usize {
        self.offsets
            .get(index)
            .copied()
            .unwrap_or_else(|| self.offsets.last().copied().unwrap_or(0))
    }
}

fn render_patch(path: &str, before: &str, edits: &[ResolvedEdit]) -> String {
    let mut patch = format!("--- {path}\n+++ {path}\n");
    let mut ranges = edits
        .iter()
        .map(|edit| full_line_range(before, edit.start, edit.end))
        .collect::<Vec<_>>();
    ranges.sort_by_key(|(start, _)| *start);
    ranges = merge_ranges(ranges);

    for (range_start, range_end) in ranges {
        let old_block = &before[range_start..range_end];
        let new_block = apply_edits_in_range(before, range_start, range_end, edits);
        let old_start_line = line_number_at(before, range_start);
        let new_start_line = new_line_number_at(before, edits, range_start);
        let old_lines = patch_lines(old_block);
        let new_lines = patch_lines(&new_block);

        patch.push_str(&format!(
            "@@ -{} +{} @@\n",
            patch_range(old_start_line, old_lines.len()),
            patch_range(new_start_line, new_lines.len())
        ));
        for line in old_lines {
            patch.push('-');
            patch.push_str(line);
            patch.push('\n');
        }
        for line in new_lines {
            patch.push('+');
            patch.push_str(line);
            patch.push('\n');
        }
    }

    patch
}

fn success_message(
    path: &str,
    before: &str,
    requested_edits: usize,
    resolved_edits: &[ResolvedEdit],
) -> String {
    let fuzzy_matches = resolved_edits.iter().filter(|edit| edit.fuzzy).count();
    let first_changed_line = resolved_edits
        .first()
        .map(|edit| line_number_at(before, edit.start))
        .unwrap_or(1);
    let mut message = format!(
        "Applied {} replacement(s) from {requested_edits} edit(s) to {path}\nFirst changed line: {first_changed_line}",
        resolved_edits.len(),
    );
    if fuzzy_matches > 0 {
        message.push_str(&format!(
            "\nFuzzy-normalized {fuzzy_matches} match(es) before applying replacements"
        ));
    }
    message.push_str("\n\nPatch:\n");
    message.push_str(&render_patch(path, before, resolved_edits));
    message
}

fn full_line_range(text: &str, start: usize, end: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut range_start = start;
    while range_start > 0 && bytes[range_start - 1] != b'\n' {
        range_start -= 1;
    }
    let mut range_end = end;
    while range_end < bytes.len() && bytes[range_end] != b'\n' {
        range_end += 1;
    }
    if range_end < bytes.len() {
        range_end += 1;
    }
    (range_start, range_end)
}

fn merge_ranges(ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => {
                *last_end = (*last_end).max(end);
                continue;
            }
            _ => {}
        }
        merged.push((start, end));
    }
    merged
}

fn apply_edits_in_range(
    before: &str,
    range_start: usize,
    range_end: usize,
    edits: &[ResolvedEdit],
) -> String {
    let mut out = String::new();
    let mut cursor = range_start;
    for edit in edits {
        if edit.start < range_start || edit.end > range_end {
            continue;
        }
        out.push_str(&before[cursor..edit.start]);
        out.push_str(&edit.new_text);
        cursor = edit.end;
    }
    out.push_str(&before[cursor..range_end]);
    out
}

fn patch_lines(block: &str) -> Vec<&str> {
    if block.is_empty() {
        return Vec::new();
    }
    let trimmed = block.strip_suffix('\n').unwrap_or(block);
    if trimmed.is_empty() {
        vec![""]
    } else {
        trimmed.split('\n').collect()
    }
}

fn patch_range(start_line: usize, line_count: usize) -> String {
    if line_count == 1 {
        start_line.to_string()
    } else {
        format!("{start_line},{line_count}")
    }
}

fn line_number_at(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn new_line_number_at(before: &str, edits: &[ResolvedEdit], byte_offset: usize) -> usize {
    let old_line = line_number_at(before, byte_offset) as isize;
    let delta = edits
        .iter()
        .filter(|edit| edit.start < byte_offset)
        .map(|edit| {
            edit.new_text.matches('\n').count() as isize
                - before[edit.start..edit.end].matches('\n').count() as isize
        })
        .sum::<isize>();
    (old_line + delta).max(1) as usize
}
