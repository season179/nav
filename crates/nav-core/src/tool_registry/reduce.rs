//! Per-tool semantic reducers that turn oversized raw output into a compact
//! model-visible view while keeping intent.
//!
//! The generic [`super::truncate`] caps protect against runaway byte/line
//! growth but throw away the *meaning* of the dropped content. A `grep` hit
//! list, a full-file read, and a bash error log each carry different signal,
//! and a deterministic head/head+tail clip ignores that.
//!
//! These reducers do the work that pure truncation can't:
//! - [`reduce_read_file`] keeps a preview, surfaces an artifact id, and
//!   suggests the next slice so the model can resume paging without
//!   re-issuing the broad call.
//! - [`reduce_code_search`] groups matches by file with counts so the model
//!   sees the *shape* of the hits before the raw lines drop out.
//! - [`reduce_bash`] preserves head, failure-looking lines pulled from the
//!   dropped middle, and tail content together, so an error buried mid-log
//!   still reaches the model.
//!
//! No LLMs here — every reducer is a pure function of its inputs so tests
//! pin the exact reduction behavior.

use std::collections::BTreeMap;

use anyhow::Result;

use super::output_accumulator::{self, ArtifactRef};
use super::truncate::{
    BoundedOutput, GREP_MAX_LINE_LENGTH, MAX_BYTES, MAX_LINES, READ_FILE_MAX_BYTES,
    READ_FILE_MAX_LINES, TruncateMode, bound, byte_prefix, truncate_line,
};

/// Result of a `read_file` reduction. When the body fit under the per-tool
/// caps the reducer passes it through unchanged and `artifact` is `None`;
/// when it didn't, `artifact` carries the stable id/path the caller can
/// surface so the model retrieves the full file with `expand_artifact` (or
/// continues paging with `read_file` + `offset`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedRead {
    pub content: String,
    pub artifact: Option<ArtifactRef>,
}

/// Lines kept in the preview when a full `read_file` overflows the cap.
/// Smaller than `READ_FILE_MAX_LINES` so the outline header, paging hint,
/// and artifact trailer comfortably fit before the byte cap clips them.
const READ_PREVIEW_LINES: usize = 480;
/// Byte ceiling for the preview body itself, leaving headroom for the
/// header/trailer text inside `READ_FILE_MAX_BYTES`.
const READ_PREVIEW_BYTES: usize = READ_FILE_MAX_BYTES - 2 * 1024;

/// Reduce a full `read_file` body. When the body fits under the per-tool
/// caps it passes through; otherwise the full bytes are persisted as an
/// artifact and the model sees a preview + outline + next-slice hint.
pub fn reduce_read_file(body: String) -> Result<ReducedRead> {
    let total_bytes = body.len();
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let total_lines = lines.len();
    if total_bytes <= READ_FILE_MAX_BYTES && total_lines <= READ_FILE_MAX_LINES {
        return Ok(ReducedRead {
            content: body,
            artifact: None,
        });
    }

    let artifact = output_accumulator::store_artifact("read", body.as_bytes())?;

    let mut preview = String::new();
    let mut kept_full_lines = 0usize;
    let mut kept_bytes = 0usize;
    let mut first_line_clipped = false;
    for line in lines.iter().take(READ_PREVIEW_LINES) {
        if kept_bytes + line.len() > READ_PREVIEW_BYTES {
            // Minified / generated files can have a single line that already
            // exceeds the preview budget. Without this branch the loop breaks
            // before appending anything and the model sees an empty preview
            // (outline "lines 1-0"), forcing an expand_artifact round-trip
            // just to see *any* content. Keep a UTF-8-safe prefix of the
            // first line in that case so the model has at least a glimpse.
            //
            // Crucially we do *not* advance `kept_full_lines` for a clipped
            // prefix — the model still needs to resume *at* line 1 to read
            // the rest of it. If we counted the partial line as kept, the
            // trailer would say "next offset 2" and a follow-up read would
            // silently skip the unshown bytes of line 1.
            if kept_full_lines == 0 {
                let room = READ_PREVIEW_BYTES.saturating_sub(kept_bytes);
                let prefix = byte_prefix(line, room);
                if !prefix.is_empty() {
                    preview.push_str(prefix);
                    first_line_clipped = prefix.len() < line.len();
                }
            }
            break;
        }
        preview.push_str(line);
        kept_bytes += line.len();
        kept_full_lines += 1;
    }
    let next_offset = if first_line_clipped {
        // Partial line 1 was shown but not consumed — resume at 1 to read
        // the rest of it.
        1
    } else {
        kept_full_lines + 1
    };
    let remaining_lines = total_lines.saturating_sub(kept_full_lines);

    let header = if first_line_clipped {
        format!(
            "[file outline: {total_lines} lines, {total_bytes} bytes; preview showing a clipped prefix of line 1]\n",
        )
    } else {
        format!(
            "[file outline: {total_lines} lines, {total_bytes} bytes; preview showing lines 1-{kept_full_lines}]\n",
        )
    };
    let mut trailer = String::new();
    if !preview.ends_with('\n') {
        trailer.push('\n');
    }
    if first_line_clipped {
        trailer.push_str(&format!(
            "[showed prefix of line 1 only ({total_lines} lines total); resume at offset 1 to read the rest of line 1]\n",
        ));
    } else {
        trailer.push_str(&format!(
            "[showed lines 1-{kept_full_lines} of {total_lines}; {remaining_lines} more lines remain; next offset {next_offset}]\n",
        ));
    }
    trailer.push_str(&format!(
        "[Artifact: {id} — call expand_artifact with artifact_id=\"{id}\" or read_file with offset={next_offset} to see the rest]\n",
        id = artifact.id,
    ));

    let mut content = String::with_capacity(header.len() + preview.len() + trailer.len());
    content.push_str(&header);
    content.push_str(&preview);
    content.push_str(&trailer);
    Ok(ReducedRead {
        content,
        artifact: Some(artifact),
    })
}

/// Cap on per-file match lines surfaced in the raw section. Mirrors the
/// previous `code_search` behavior so the reducer doesn't regress the
/// per-file output shape.
const CODE_SEARCH_MAX_MATCHES: usize = 100;
/// Threshold for emitting the by-file grouping header. Below this the raw
/// output is short enough to read directly without a summary.
const CODE_SEARCH_GROUP_MIN_MATCHES: usize = 10;
/// Cap on per-file summary rows. A grep over a vendored tree could hit
/// thousands of files and turn the "summary" itself into a context flood;
/// the top-N highest-count files cover almost all signal.
const CODE_SEARCH_SUMMARY_MAX_FILES: usize = 50;
/// Byte ceiling for the summary section itself. Even with the row cap
/// above, 50 rows of very long monorepo paths could still rival
/// `MAX_BYTES` and saturate the raw-budget calculation below. Once the
/// summary hits this ceiling, remaining rows collapse into the
/// "[+N more files omitted]" trailer.
const CODE_SEARCH_SUMMARY_MAX_BYTES: usize = MAX_BYTES / 4;

/// Reduce `rg`-style stdout to a model-friendly view. When matches span
/// multiple files (or there are many of them), a `[grouped by file: ...]`
/// summary is prepended with per-file match counts; the raw `file:line:hit`
/// rows follow under the same 100-match / `MAX_BYTES` cap as before. Empty
/// stdout (no matches) is preserved verbatim so callers see "no hits".
pub fn reduce_code_search(stdout: &str) -> String {
    if stdout.is_empty() {
        return String::new();
    }

    let raw_lines: Vec<&str> = stdout.lines().collect();
    let total_matches = raw_lines.len();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for line in &raw_lines {
        let file = file_prefix(line).unwrap_or("(unknown)");
        *counts.entry(file).or_insert(0) += 1;
    }
    let file_count = counts.len();
    let show_summary = total_matches >= CODE_SEARCH_GROUP_MIN_MATCHES && file_count >= 2;

    // The raw block budget reserves a small headroom for the trailer the
    // truncate path used to emit and for any summary header above it.
    const TRAILER_RESERVE: usize = 64;
    let mut out = String::new();
    if show_summary {
        out.push_str(&format!(
            "[grouped by file: {file_count} files, {total_matches} matches]\n",
        ));
        // Sort by descending count, then by file path for determinism.
        let mut by_count: Vec<(&str, usize)> = counts.into_iter().collect();
        by_count.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        // Cap the summary section by both row count *and* byte size: a
        // grep over a large vendored tree could otherwise produce thousands
        // of rows (or, with the row cap alone, ~50 rows of very long
        // monorepo paths) and saturate the raw-budget calculation below.
        let summary_start = out.len();
        let row_cap = by_count.len().min(CODE_SEARCH_SUMMARY_MAX_FILES);
        let mut summarized = 0usize;
        for (file, count) in by_count.iter().take(row_cap) {
            let label = if *count == 1 { "match" } else { "matches" };
            let row = format!("  {file}: {count} {label}\n");
            if (out.len() - summary_start) + row.len() > CODE_SEARCH_SUMMARY_MAX_BYTES {
                break;
            }
            out.push_str(&row);
            summarized += 1;
        }
        let omitted_files = by_count.len().saturating_sub(summarized);
        if omitted_files > 0 {
            out.push_str(&format!(
                "  [+{omitted_files} more files omitted from summary]\n",
            ));
        }
        out.push('\n');
    }

    let raw_byte_budget = MAX_BYTES
        .saturating_sub(TRAILER_RESERVE)
        .saturating_sub(out.len());
    let mut raw_bytes_added = 0usize;
    let mut byte_budget_hit = false;
    let mut raw_iter = raw_lines.iter();
    for line in raw_iter.by_ref().take(CODE_SEARCH_MAX_MATCHES) {
        let (clipped, _) = truncate_line(line, GREP_MAX_LINE_LENGTH);
        if raw_bytes_added + clipped.len() + 1 > raw_byte_budget {
            byte_budget_hit = true;
            break;
        }
        out.push_str(&clipped);
        out.push('\n');
        raw_bytes_added += clipped.len() + 1;
    }
    let dropped = raw_iter.count() + usize::from(byte_budget_hit);
    if dropped > 0 {
        out.push_str(&format!(
            "[truncated {dropped} of {total_matches} matches]\n",
        ));
    }
    out
}

/// Extract the `file` prefix from an `rg`-style `file:line:match` row.
/// Falls back to `None` when the line doesn't carry the expected shape so
/// the caller can bucket those rows under `(unknown)` rather than crashing.
fn file_prefix(line: &str) -> Option<&str> {
    let colon = line.find(':')?;
    Some(&line[..colon])
}

/// Lines from the dropped middle of a bash log to lift back into the
/// visible window when they look like failure indicators. Anything past
/// this cap stays in the artifact / on-disk spill.
const BASH_FAILURE_LINES_MAX: usize = 20;
/// Byte ceiling for the failure section. Keeps the section small enough
/// that the head+tail body still dominates the visible window.
const BASH_FAILURE_BYTES_MAX: usize = MAX_BYTES / 8;

/// Reduce raw bash output: apply the existing head+tail bound, then —
/// when the bound fires — lift up to [`BASH_FAILURE_LINES_MAX`] lines
/// that look like errors/panics/tracebacks out of the dropped middle and
/// surface them between the head and the truncated marker. Deterministic
/// pattern set; no LLM.
pub fn reduce_bash(output: String, head_lines: usize) -> BoundedOutput {
    let bounded = bound(output.clone(), TruncateMode::HeadTail { head_lines });
    if !bounded.truncated {
        return bounded;
    }
    let failures = extract_failure_lines(&output, head_lines);
    if failures.is_empty() {
        return bounded;
    }
    let Some(marker_pos) = bounded.content.find("\n[truncated") else {
        return bounded;
    };
    let failures_bytes: usize = failures.iter().map(|line| line.len() + 1).sum();
    let mut content = String::with_capacity(
        bounded.content.len() + failures_bytes + "\n[failure lines from dropped middle:]\n".len(),
    );
    content.push_str(&bounded.content[..marker_pos]);
    content.push_str("\n[failure lines from dropped middle:]\n");
    for line in failures {
        content.push_str(&line);
        if !content.ends_with('\n') {
            content.push('\n');
        }
    }
    content.push_str(&bounded.content[marker_pos..]);
    BoundedOutput {
        content,
        truncated: true,
        kept_full_lines: bounded.kept_full_lines,
    }
}

/// Walk the dropped middle (lines past `head_lines`, ignoring the trailing
/// `MAX_LINES - head_lines` lines that the tail keeps) and pick up to
/// [`BASH_FAILURE_LINES_MAX`] failure-looking lines under
/// [`BASH_FAILURE_BYTES_MAX`] bytes. Pure function on the raw output —
/// callers test it independently from the bound mechanics.
fn extract_failure_lines(output: &str, head_lines: usize) -> Vec<String> {
    let head_lines = head_lines.min(MAX_LINES);
    let tail_budget = MAX_LINES.saturating_sub(head_lines);
    let lines: Vec<&str> = output.split_inclusive('\n').collect();
    let total = lines.len();
    if total <= head_lines + tail_budget {
        return Vec::new();
    }
    let middle_start = head_lines;
    let middle_end = total.saturating_sub(tail_budget);
    let mut out: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for line in &lines[middle_start..middle_end] {
        if !looks_like_failure(line) {
            continue;
        }
        let (clipped, _) = truncate_line(line.trim_end_matches('\n'), GREP_MAX_LINE_LENGTH);
        if bytes + clipped.len() + 1 > BASH_FAILURE_BYTES_MAX {
            break;
        }
        bytes += clipped.len() + 1;
        out.push(clipped);
        if out.len() >= BASH_FAILURE_LINES_MAX {
            break;
        }
    }
    out
}

/// Deterministic failure-pattern probe. Case-insensitive on a small set
/// of substrings that consistently mark compiler/runtime/test errors.
/// Substrings (not regex) so the cost stays predictable on long lines.
fn looks_like_failure(line: &str) -> bool {
    // ASCII-lowercase the candidate once. Allocating per scanned line is
    // cheaper than maintaining a parallel iterator of lowercased prefixes
    // and keeps the matcher straightforward to extend.
    let lower = line.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "error:",
        "error[",
        " error ",
        "internal compiler error",
        "panicked at",
        "panic:",
        "traceback",
        "exception:",
        " exception ",
        " failed",
        "failed:",
        "failure:",
        "assertion failed",
        "fatal:",
        "segmentation fault",
        "undefined reference",
    ];
    NEEDLES.iter().any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── read_file reducer ─────────────────────────────────────────

    #[test]
    fn reduce_read_file_passes_short_body_through_unchanged() {
        let body = "alpha\nbeta\n".to_string();
        let result = reduce_read_file(body.clone()).unwrap();
        assert!(result.artifact.is_none(), "small read should not spill");
        assert_eq!(result.content, body);
    }

    #[test]
    fn reduce_read_file_emits_outline_preview_and_artifact_for_oversize_body() {
        // Build > READ_FILE_MAX_LINES lines so the cap fires on line count
        // alone; this also keeps the test fast vs. a giant byte payload.
        let body = (0..(READ_FILE_MAX_LINES + 500))
            .map(|i| format!("line{i}\n"))
            .collect::<String>();
        let total_lines = body.lines().count();
        let total_bytes = body.len();

        let result = reduce_read_file(body).unwrap();
        let artifact = result
            .artifact
            .as_ref()
            .expect("oversize read should spill");
        assert!(
            result
                .content
                .starts_with(&format!("[file outline: {total_lines} lines,")),
            "missing outline header: {}",
            &result.content[..result.content.len().min(120)]
        );
        assert!(result.content.contains(&total_bytes.to_string()));
        assert!(
            result
                .content
                .contains(&format!("[Artifact: {}", artifact.id))
        );
        assert!(result.content.contains("call expand_artifact"));
        assert!(result.content.contains("read_file with offset"));
        // Preview includes the very first lines and excludes the tail.
        assert!(result.content.contains("line0\n"));
        assert!(
            !result
                .content
                .contains(&format!("line{}\n", total_lines - 1))
        );
        // The model-visible view fits inside the per-tool byte cap.
        assert!(
            result.content.len() <= READ_FILE_MAX_BYTES,
            "preview was {} bytes",
            result.content.len()
        );

        // Round-trip: the stored artifact reads back to the exact original.
        let back = output_accumulator::read_artifact(&artifact.id).unwrap();
        let _ = std::fs::remove_file(&artifact.path);
        assert_eq!(back.lines().count(), total_lines);
        assert!(back.starts_with("line0\n"));
    }

    #[test]
    fn reduce_read_file_next_offset_resumes_after_preview() {
        let body = (0..(READ_FILE_MAX_LINES + 200))
            .map(|i| format!("row{i}\n"))
            .collect::<String>();
        let result = reduce_read_file(body).unwrap();
        let artifact = result.artifact.as_ref().unwrap();
        // The next-offset hint must be one past the last preview line.
        let preview_lines = result
            .content
            .lines()
            .filter(|l| l.starts_with("row"))
            .count();
        assert!(
            result
                .content
                .contains(&format!("next offset {}", preview_lines + 1))
        );
        let _ = std::fs::remove_file(&artifact.path);
    }

    #[test]
    fn reduce_read_file_keeps_prefix_when_first_line_exceeds_preview_budget() {
        // Minified bundle: one line, longer than READ_PREVIEW_BYTES. Without
        // the first-line salvage path the preview would be empty and the
        // outline would say "lines 1-0", forcing an expand_artifact round-trip
        // before the model could see any content. The salvage must NOT
        // advance the line offset — the trailer must still resume at offset 1
        // so the model re-reads the unshown bytes of line 1 instead of
        // skipping past them.
        let body = "x".repeat(READ_FILE_MAX_BYTES + 4_096);
        let result = reduce_read_file(body).unwrap();
        let artifact = result.artifact.as_ref().unwrap();
        assert!(
            result.content.contains("clipped prefix of line 1"),
            "header must surface that the first line was clipped — got:\n{}",
            result.content
        );
        assert!(
            !result.content.contains("next offset 2"),
            "next offset must NOT advance past the partially-shown line 1; got:\n{}",
            result.content
        );
        assert!(
            result.content.contains("resume at offset 1"),
            "trailer must direct the model to re-read line 1; got:\n{}",
            result.content
        );
        // The clipped prefix should fit comfortably under the byte ceiling
        // but still be substantial (not empty / not a token).
        let preview_x_count = result.content.matches('x').count();
        assert!(
            preview_x_count > 1024,
            "expected a substantive prefix of the long line, got {preview_x_count} bytes"
        );
        let _ = std::fs::remove_file(&artifact.path);
    }

    // ── code_search reducer ────────────────────────────────────────

    #[test]
    fn reduce_code_search_passes_empty_through() {
        assert_eq!(reduce_code_search(""), "");
    }

    #[test]
    fn reduce_code_search_skips_summary_for_few_matches() {
        let stdout = "a.rs:1:alpha\nb.rs:1:beta\n";
        let result = reduce_code_search(stdout);
        assert!(!result.contains("grouped by file"));
        assert!(result.contains("a.rs:1:alpha"));
        assert!(result.contains("b.rs:1:beta"));
    }

    #[test]
    fn reduce_code_search_groups_matches_by_file_with_counts() {
        // 12 matches across 2 files: above the grouping threshold, so the
        // summary header surfaces match counts per file before the raw rows.
        let mut stdout = String::new();
        for i in 0..8 {
            stdout.push_str(&format!("src/foo.rs:{i}:hit\n"));
        }
        for i in 0..4 {
            stdout.push_str(&format!("src/bar.rs:{i}:hit\n"));
        }
        let result = reduce_code_search(&stdout);
        assert!(
            result.starts_with("[grouped by file: 2 files, 12 matches]\n"),
            "missing summary header: {}",
            &result[..result.len().min(80)]
        );
        // Counts are listed; descending order so foo (8) precedes bar (4).
        let foo_at = result.find("src/foo.rs: 8 matches").expect("foo summary");
        let bar_at = result.find("src/bar.rs: 4 matches").expect("bar summary");
        assert!(foo_at < bar_at);
        // The raw rows still follow the summary.
        assert!(result.contains("src/foo.rs:0:hit"));
        assert!(result.contains("src/bar.rs:0:hit"));
    }

    #[test]
    fn reduce_code_search_caps_to_max_matches_with_trailer() {
        let mut stdout = String::new();
        for i in 0..200 {
            stdout.push_str(&format!("file.rs:{}:hit-{i}\n", i + 1));
        }
        let result = reduce_code_search(&stdout);
        let match_lines = result.lines().filter(|l| l.starts_with("file.rs:")).count();
        assert_eq!(match_lines, 100);
        assert!(result.contains("[truncated 100 of 200 matches]"));
        // Single file → no grouping header.
        assert!(!result.contains("grouped by file"));
    }

    #[test]
    fn reduce_code_search_clips_long_match_lines() {
        let payload = "x".repeat(2000);
        let stdout = format!("file.rs:1:{payload}\n");
        let result = reduce_code_search(&stdout);
        assert!(result.contains("... [truncated]"));
    }

    #[test]
    fn reduce_code_search_summary_caps_by_bytes_with_long_paths() {
        // A monorepo with deep paths can blow past the row cap on bytes
        // even when only a handful of files match. Without the byte ceiling
        // the summary alone could rival MAX_BYTES.
        let long_path = format!("very/{}", "deep/".repeat(120));
        let mut stdout = String::new();
        // 60 files all with long paths — more than the row cap of 50, so
        // we exercise both caps interacting.
        for i in 0..60 {
            stdout.push_str(&format!("{long_path}file_{i:03}.rs:1:hit\n"));
        }
        let result = reduce_code_search(&stdout);
        assert!(
            result.len() <= MAX_BYTES,
            "reducer output {} bytes exceeds MAX_BYTES {}",
            result.len(),
            MAX_BYTES
        );
        // Byte cap must trigger before the row cap when paths are huge.
        let summary_section = result.split("\n\n").next().unwrap_or_default();
        assert!(
            summary_section.len() <= CODE_SEARCH_SUMMARY_MAX_BYTES + 2_048,
            "summary section ({} bytes) must stay near the byte cap ({} bytes)",
            summary_section.len(),
            CODE_SEARCH_SUMMARY_MAX_BYTES
        );
        assert!(
            result.contains("more files omitted from summary"),
            "byte-cap path must still surface the omission notice"
        );
    }

    #[test]
    fn reduce_code_search_caps_summary_when_many_files_match() {
        // A grep over a large tree could hit thousands of files. Without a
        // summary cap, the per-file summary section would balloon, saturate
        // `raw_byte_budget` to zero, and ship an oversized output back to
        // the model — regressing the MAX_BYTES guard.
        let mut stdout = String::new();
        let file_count = 2_000usize;
        for i in 0..file_count {
            stdout.push_str(&format!("path/to/file_{i:04}.rs:1:alpha\n"));
        }
        let result = reduce_code_search(&stdout);
        // Header reports the true file count.
        assert!(result.contains(&format!("[grouped by file: {file_count} files")));
        // But the summary itself caps the rows. Per-file rows start with
        // two spaces and end with " match" / " matches"; the header and
        // truncation trailer are bracketed so they don't match this shape.
        let summary_rows = result
            .lines()
            .filter(|l| {
                l.starts_with("  ")
                    && !l.starts_with("  [")
                    && (l.ends_with(" match") || l.ends_with(" matches"))
            })
            .count();
        assert!(
            summary_rows <= CODE_SEARCH_SUMMARY_MAX_FILES,
            "summary should be capped at {CODE_SEARCH_SUMMARY_MAX_FILES} rows, got {summary_rows}"
        );
        // And the trailing notice surfaces the omission so the model can
        // request a narrower search.
        assert!(
            result.contains("more files omitted from summary"),
            "summary cap must surface the omitted-file count"
        );
        // Final output stays under the global MAX_BYTES ceiling so it
        // cannot flood the next prompt.
        assert!(
            result.len() <= MAX_BYTES,
            "reducer output {} bytes exceeds MAX_BYTES {}",
            result.len(),
            MAX_BYTES
        );
    }

    // ── bash reducer ───────────────────────────────────────────────

    #[test]
    fn reduce_bash_passes_short_output_through() {
        let result = reduce_bash("status: 0\nstdout: hello\n".to_string(), 200);
        assert!(!result.truncated);
        assert!(result.content.contains("hello"));
    }

    #[test]
    fn reduce_bash_lifts_failure_lines_from_dropped_middle() {
        // Build a log where the first head_lines are benign, the middle
        // contains a clear "error:" line, and the tail is benign again.
        let head_lines = 200;
        let mut body = String::new();
        for i in 0..head_lines {
            body.push_str(&format!("noise-head-{i}\n"));
        }
        // Middle: lots of filler with a single failure line buried in it.
        for i in 0..3000 {
            if i == 1500 {
                body.push_str("error: compilation failed at module foo\n");
            } else {
                body.push_str(&format!("noise-middle-{i}\n"));
            }
        }
        for i in 0..(MAX_LINES - head_lines) {
            body.push_str(&format!("noise-tail-{i}\n"));
        }

        let result = reduce_bash(body, head_lines);
        assert!(result.truncated);
        assert!(
            result
                .content
                .contains("[failure lines from dropped middle:]"),
            "missing failure section: {}",
            &result.content[..result.content.len().min(200)]
        );
        assert!(
            result
                .content
                .contains("error: compilation failed at module foo"),
            "failure line not lifted into view"
        );
        // Head and tail are still present so the model retains context.
        assert!(result.content.contains("noise-head-0\n"));
        assert!(
            result
                .content
                .contains(&format!("noise-tail-{}\n", MAX_LINES - head_lines - 1)),
        );
        // Bound still fires after the enrichment.
        assert!(result.content.contains("[truncated"));
    }

    #[test]
    fn reduce_bash_skips_failure_section_when_middle_has_none() {
        let head_lines = 200;
        let mut body = String::new();
        for i in 0..head_lines {
            body.push_str(&format!("noise-head-{i}\n"));
        }
        for i in 0..3000 {
            body.push_str(&format!("noise-middle-{i}\n"));
        }
        for i in 0..(MAX_LINES - head_lines) {
            body.push_str(&format!("noise-tail-{i}\n"));
        }
        let result = reduce_bash(body, head_lines);
        assert!(result.truncated);
        assert!(!result.content.contains("[failure lines"));
    }

    #[test]
    fn reduce_bash_handles_multiple_failure_patterns() {
        let head_lines = 200;
        let mut body = String::new();
        for i in 0..head_lines {
            body.push_str(&format!("noise-head-{i}\n"));
        }
        body.push_str("thread main panicked at 'oops'\n");
        body.push_str("Traceback (most recent call last):\n");
        body.push_str("AssertionError: assertion failed\n");
        body.push_str("FATAL: connection refused\n");
        for i in 0..3000 {
            body.push_str(&format!("noise-middle-{i}\n"));
        }
        for i in 0..(MAX_LINES - head_lines) {
            body.push_str(&format!("noise-tail-{i}\n"));
        }
        let result = reduce_bash(body, head_lines);
        assert!(
            result
                .content
                .contains("[failure lines from dropped middle:]")
        );
        assert!(result.content.contains("panicked at"));
        assert!(result.content.contains("Traceback"));
        assert!(result.content.contains("assertion failed"));
        assert!(result.content.contains("FATAL: connection refused"));
    }

    #[test]
    fn reduce_bash_caps_failure_section_lines() {
        let head_lines = 200;
        let mut body = String::new();
        for i in 0..head_lines {
            body.push_str(&format!("noise-head-{i}\n"));
        }
        // 60 failure-looking lines in the middle, more than the
        // BASH_FAILURE_LINES_MAX cap permits.
        for i in 0..60 {
            body.push_str(&format!("error: middle failure {i}\n"));
        }
        for i in 0..3000 {
            body.push_str(&format!("noise-middle-{i}\n"));
        }
        for i in 0..(MAX_LINES - head_lines) {
            body.push_str(&format!("noise-tail-{i}\n"));
        }
        let result = reduce_bash(body, head_lines);
        // Pull just the failure section to inspect its line count.
        let section_start = result
            .content
            .find("[failure lines from dropped middle:]\n")
            .expect("failure section present");
        let section_end_rel = result.content[section_start..]
            .find("\n[truncated")
            .expect("marker after section");
        let section_body = &result.content[section_start..section_start + section_end_rel];
        let kept = section_body
            .lines()
            .filter(|l| l.starts_with("error:"))
            .count();
        assert!(
            kept <= BASH_FAILURE_LINES_MAX,
            "failure section kept {kept} lines",
        );
    }

    #[test]
    fn looks_like_failure_matches_common_patterns() {
        assert!(looks_like_failure("error: something broke\n"));
        assert!(looks_like_failure("ERROR: shouting works\n"));
        assert!(looks_like_failure("error[E0599]: rust diagnostic\n"));
        assert!(looks_like_failure("thread 'main' panicked at file.rs:1\n"));
        assert!(looks_like_failure("Traceback (most recent call last):\n"));
        assert!(looks_like_failure("Exception: boom\n"));
        assert!(looks_like_failure("test_foo ... FAILED\n"));
        assert!(looks_like_failure("assertion failed: lhs == rhs\n"));
        assert!(looks_like_failure("fatal: cannot resolve\n"));
        assert!(looks_like_failure("segmentation fault\n"));
        assert!(looks_like_failure("undefined reference to `bar`\n"));
        assert!(!looks_like_failure("All tests passed in 12s\n"));
        assert!(!looks_like_failure("ok 12 - no issues\n"));
    }

    #[test]
    fn read_preview_constants_fit_inside_read_file_cap() {
        // The reducer relies on these being small enough that the outline
        // header and artifact trailer fit under READ_FILE_MAX_BYTES.
        const _: () = assert!(READ_PREVIEW_BYTES < READ_FILE_MAX_BYTES);
        const _: () = assert!(READ_PREVIEW_LINES <= READ_FILE_MAX_LINES);
    }
}
