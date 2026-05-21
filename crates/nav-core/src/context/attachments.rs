use base64::Engine;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::agent_loop::UserAttachment;
use crate::guardrails::protected::is_protected_read;
use crate::tool_registry::truncate::{
    READ_FILE_MAX_BYTES, READ_FILE_MAX_LINES, TruncateMode, bound_with_limits,
};

/// Build the `content` part of a Responses API user message. Plain text turns
/// stay as a single string (the historical shape); when attachments or
/// submit-time `@file` mentions are present, return an array of typed content
/// parts so the Responses API sees `input_text` alongside `input_image`.
/// Attachments that fail to load are dropped silently; submit-time mentions
/// surface inline notes so the model knows what the user tried to include.
pub(crate) fn build_user_content(
    prompt: &str,
    mention_source: Option<&str>,
    attachments: &[UserAttachment],
    cwd: &Path,
) -> Value {
    let mention_parts = resolve_submit_file_mentions(mention_source.unwrap_or(prompt), cwd);
    if attachments.is_empty() && mention_parts.is_empty() {
        return Value::String(prompt.to_string());
    }
    // Canonicalize cwd once; resolve_workspace_path used to do this per
    // attachment, which on a turn with N files meant N stat-walks of the
    // workspace root.
    let cwd_canonical = cwd.canonicalize().ok();
    let mut parts: Vec<Value> = Vec::with_capacity(1 + attachments.len() + mention_parts.len());
    parts.push(input_text_part(prompt));
    for attach in attachments {
        match attach {
            UserAttachment::Image { path } => {
                if let Some(canonical) = resolve_workspace_path(cwd_canonical.as_deref(), cwd, path)
                    && let Some(data_uri) = encode_image_data_uri(&canonical)
                {
                    parts.push(json!({
                        "type": "input_image",
                        "image_url": data_uri,
                    }));
                }
            }
            UserAttachment::File { path } => {
                if let Some(text) = load_file_attachment(cwd_canonical.as_deref(), cwd, path) {
                    parts.push(input_text_part(text));
                }
            }
        }
    }
    for text in mention_parts {
        parts.push(input_text_part(text));
    }
    Value::Array(parts)
}

fn input_text_part(text: impl Into<String>) -> Value {
    json!({
        "type": "input_text",
        "text": text.into(),
    })
}

/// Canonicalize `rel` against `cwd` and require workspace containment. A
/// `..`-laden path or a symlink that escapes would otherwise let us
/// silently include arbitrary files (e.g. `~/.ssh/id_rsa`) in the request.
/// `cwd_canonical` is the pre-canonicalized cwd from the caller - passing
/// `None` falls back to canonicalizing per call, used by code paths that
/// don't have the canonical form handy.
fn resolve_workspace_path(cwd_canonical: Option<&Path>, cwd: &Path, rel: &Path) -> Option<PathBuf> {
    let abs = if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        cwd.join(rel)
    };
    let canonical = abs.canonicalize().ok()?;
    let cwd_canonical = match cwd_canonical {
        Some(p) => p.to_path_buf(),
        None => cwd.canonicalize().ok()?,
    };
    canonical.starts_with(&cwd_canonical).then_some(canonical)
}

fn encode_image_data_uri(canonical: &Path) -> Option<String> {
    let bytes = std::fs::read(canonical).ok()?;
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "png".to_string());
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

fn resolve_submit_file_mentions(source: &str, cwd: &Path) -> Vec<String> {
    let mentions = scan_submit_file_mentions(source);
    if mentions.is_empty() {
        return Vec::new();
    }
    let cwd_canonical = cwd.canonicalize().ok();
    mentions
        .into_iter()
        .filter_map(|mention| resolve_submit_file_mention(&mention, cwd_canonical.as_deref(), cwd))
        .collect()
}

fn scan_submit_file_mentions(source: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let mut search_from = 0;
    while let Some(relative_at) = source[search_from..].find('@') {
        let at = search_from + relative_at;
        if at > 0
            && source[..at]
                .chars()
                .next_back()
                .is_some_and(|ch| !ch.is_whitespace())
        {
            search_from = at + '@'.len_utf8();
            continue;
        }
        let token_start = at + '@'.len_utf8();
        let Some((token, next)) = parse_submit_file_mention_token(source, token_start) else {
            search_from = token_start;
            continue;
        };
        mentions.push(token);
        search_from = next;
    }
    mentions
}

fn parse_submit_file_mention_token(source: &str, start: usize) -> Option<(String, usize)> {
    let first = source[start..].chars().next()?;
    if matches!(first, '"' | '\'') {
        let content_start = start + first.len_utf8();
        let rest = &source[content_start..];
        let end_relative = rest.find(first)?;
        let content_end = content_start + end_relative;
        let token = source[content_start..content_end].trim();
        return (!token.is_empty()).then(|| (token.to_string(), content_end + first.len_utf8()));
    }

    if first.is_whitespace() {
        return None;
    }
    let mut end = source.len();
    for (relative, ch) in source[start..].char_indices() {
        if ch.is_whitespace() {
            end = start + relative;
            break;
        }
    }
    let raw = &source[start..end];
    let token = trim_trailing_mention_punctuation(raw);
    (!token.is_empty()).then(|| (token.to_string(), end))
}

fn trim_trailing_mention_punctuation(raw: &str) -> &str {
    raw.trim_end_matches([',', ';', ':', '!', '?', ')', ']', '}'])
        .trim_end_matches('.')
}

fn resolve_submit_file_mention(
    token: &str,
    cwd_canonical: Option<&Path>,
    cwd: &Path,
) -> Option<String> {
    let path = Path::new(token);
    if path.is_absolute() {
        return Some(file_mention_note(
            token,
            "not resolved: absolute paths are not allowed",
        ));
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Some(file_mention_note(
            token,
            "not resolved: parent directory traversal is not allowed",
        ));
    }

    if path_has_directory_component(path) || cwd.join(path).exists() {
        return Some(resolve_exact_file_mention(token, path, cwd_canonical, cwd));
    }

    let basename_matches = find_workspace_files_named(cwd, token);
    match basename_matches.as_slice() {
        [] if path_looks_like_file(token) => Some(file_mention_note(
            token,
            "not resolved: no such workspace file",
        )),
        [] => None,
        [matched] => {
            let display = matched.to_string_lossy();
            Some(resolve_exact_file_mention(
                display.as_ref(),
                matched,
                cwd_canonical,
                cwd,
            ))
        }
        matches => Some(ambiguous_file_mention_note(token, matches)),
    }
}

fn resolve_exact_file_mention(
    token: &str,
    rel: &Path,
    cwd_canonical: Option<&Path>,
    cwd: &Path,
) -> String {
    let display = rel.display().to_string();
    let Some(canonical) = resolve_workspace_path(cwd_canonical, cwd, rel) else {
        return file_mention_note(token, "not resolved: no such workspace file");
    };
    if canonical.is_dir() {
        return file_mention_note(token, "not resolved: path is a directory");
    }
    if !canonical.is_file() {
        return file_mention_note(token, "not resolved: path is not a regular file");
    }
    if is_protected_read(rel) || is_protected_read(&canonical) {
        return file_mention_note(
            token,
            "refused: protected file reads require explicit approval",
        );
    }
    load_file_attachment_from_canonical(&display, &canonical)
}

fn path_has_directory_component(path: &Path) -> bool {
    path.components().count() > 1
}

fn path_looks_like_file(token: &str) -> bool {
    token.contains('.') || token.starts_with('.') || token.contains('/')
}

fn find_workspace_files_named(cwd: &Path, name: &str) -> Vec<PathBuf> {
    const MAX_BASENAME_MATCHES: usize = 8;
    let mut matches = Vec::new();
    let walker = WalkBuilder::new(cwd).build();
    for entry in walker.flatten() {
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        if entry.file_name().to_string_lossy() != name {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(cwd) {
            matches.push(rel.to_path_buf());
            if matches.len() >= MAX_BASENAME_MATCHES {
                break;
            }
        }
    }
    matches
}

fn ambiguous_file_mention_note(token: &str, matches: &[PathBuf]) -> String {
    let listed = matches
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    file_mention_note(
        token,
        &format!("ambiguous: multiple files match this name ({listed})"),
    )
}

fn file_mention_note(token: &str, message: &str) -> String {
    format!("<file mention: @{token}>\n[{message}]\n</file mention>")
}

/// Cap on how many bytes a single file attachment may pull off disk before
/// truncation. `READ_FILE_MAX_BYTES + 1` so a file at exactly the cap still has the
/// next byte sampled - the marker would otherwise lie about whether
/// anything was dropped.
const FILE_ATTACHMENT_READ_CAP: u64 = (READ_FILE_MAX_BYTES as u64) + 1;

/// Render a `File` attachment as a fenced block. UTF-8 only; binary bodies
/// fall back to a note. We read at most `FILE_ATTACHMENT_READ_CAP` bytes so
/// a 1 GB attachment doesn't pull 1 GB into memory just to discard the
/// tail. Symlink reaches into protected paths are refused inline even
/// after gate approval - matches `read_file`'s symlink-bypass check.
fn load_file_attachment(cwd_canonical: Option<&Path>, cwd: &Path, rel: &Path) -> Option<String> {
    let rel_display = rel.display().to_string();
    let canonical = resolve_workspace_path(cwd_canonical, cwd, rel)?;
    if is_protected_read(&canonical) && !is_protected_read(rel) {
        return Some(format!(
            "<attached file: {rel_display}>\n[refused: path resolves via symlink to a protected secret]\n</attached>",
        ));
    }
    Some(load_file_attachment_from_canonical(
        &rel_display,
        &canonical,
    ))
}

fn load_file_attachment_from_canonical(rel_display: &str, canonical: &Path) -> String {
    let file = match std::fs::File::open(canonical) {
        Ok(file) => file,
        Err(err) => {
            return format!(
                "<attached file: {rel_display}>\n[skipped: failed to open file: {err}]\n</attached>",
            );
        }
    };
    let mut bytes = Vec::new();
    if let Err(err) = file.take(FILE_ATTACHMENT_READ_CAP).read_to_end(&mut bytes) {
        return format!(
            "<attached file: {rel_display}>\n[skipped: failed to read file: {err}]\n</attached>",
        );
    }
    let body = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            return format!(
                "<attached file: {rel_display}>\n[skipped: file is not valid UTF-8 ({} bytes read)]\n</attached>",
                err.into_bytes().len()
            );
        }
    };
    let bounded = bound_with_limits(
        body,
        TruncateMode::Head,
        READ_FILE_MAX_LINES,
        READ_FILE_MAX_BYTES,
    )
    .content;
    format!("<attached file: {rel_display}>\n{bounded}\n</attached>",)
}
