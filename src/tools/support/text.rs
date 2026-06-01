//! Text-file byte helpers shared by editing tools.

pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
    Cr,
}

pub fn strip_utf8_bom(bytes: &[u8]) -> (bool, &[u8]) {
    if bytes.starts_with(UTF8_BOM) {
        (true, &bytes[UTF8_BOM.len()..])
    } else {
        (false, bytes)
    }
}

pub fn detect_line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => return LineEnding::Crlf,
            b'\r' => return LineEnding::Cr,
            b'\n' => return LineEnding::Lf,
            _ => {}
        }
    }
    LineEnding::Lf
}

pub fn normalize_line_endings_to_lf(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

pub fn bytes_preserving_file_style(text: &str, had_bom: bool, line_ending: LineEnding) -> Vec<u8> {
    let mut output_bytes = Vec::new();
    if had_bom {
        output_bytes.extend_from_slice(UTF8_BOM);
    }
    output_bytes.extend_from_slice(restore_line_endings(text, line_ending).as_bytes());
    output_bytes
}

fn restore_line_endings(text: &str, line_ending: LineEnding) -> String {
    match line_ending {
        LineEnding::Lf => text.to_owned(),
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Cr => text.replace('\n', "\r"),
    }
}
