//! Tiny shell decomposer for the `bash` tool.
//!
//! Goal: split a command string into a pipeline of argv vectors that the
//! classifier can evaluate. We are NOT building a real shell; anything we
//! can't confidently understand returns `Unparseable` so the caller can
//! fail-safe (treat as needing approval).
//!
//! Inspired by codex's `parse_shell_lc_plain_commands` but stripped of the
//! sh -c / bash -lc unwrapper since nav's bash tool receives the inner
//! command string directly.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellParseError {
    Unparseable,
}

impl std::fmt::Display for ShellParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellParseError::Unparseable => {
                write!(f, "command uses a shell construct nav cannot statically analyze")
            }
        }
    }
}

impl std::error::Error for ShellParseError {}

/// Decomposes a shell command string into a sequence of argvs.
///
/// Splits on top-level `&&`, `||`, `;`, and `|`. Quoted strings (`'…'` and
/// `"…"`) are tokenized literally. Anything that uses command substitution
/// (`\``, `$(`), redirections (`>`, `<`, `>>`, `<<`), or process substitution
/// (`<(`, `>(`) returns `Unparseable` — the caller should treat that as
/// "needs approval" rather than "safe".
///
/// An empty or whitespace-only input yields an empty vec (no error).
pub fn parse_command_pipeline(command: &str) -> Result<Vec<Vec<String>>, ShellParseError> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(vec![]);
    }
    reject_dangerous_constructs(trimmed)?;
    let segments = split_top_level(trimmed)?;
    let mut argvs = Vec::with_capacity(segments.len());
    for segment in segments {
        let argv = tokenize(segment.trim())?;
        if !argv.is_empty() {
            argvs.push(argv);
        }
    }
    Ok(argvs)
}

fn reject_dangerous_constructs(command: &str) -> Result<(), ShellParseError> {
    // Walk the string once, tracking quote state, and bail if we hit any
    // construct we don't want to parse for the classifier. Double-quoted
    // strings undergo expansion in real bash, so `$(...)` and backticks
    // inside `"..."` still execute commands — we reject those too.
    let bytes = command.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match (quote, b) {
            (Some(q), c) if c == q => quote = None,
            (Some(b'\''), _) => {} // single-quoted: literal
            (Some(b'"'), b'\\') => i += 1, // escape inside double quotes
            (Some(b'"'), b'`') => return Err(ShellParseError::Unparseable),
            (Some(b'"'), b'$') if peek(bytes, i + 1) == Some(b'(') => {
                return Err(ShellParseError::Unparseable);
            }
            (Some(_), _) => {} // any other char inside any quote
            (None, b'\'') | (None, b'"') => quote = Some(b),
            (None, b'`') => return Err(ShellParseError::Unparseable),
            (None, b'$') if peek(bytes, i + 1) == Some(b'(') => {
                return Err(ShellParseError::Unparseable);
            }
            (None, b'<') if peek(bytes, i + 1) == Some(b'(') => {
                return Err(ShellParseError::Unparseable);
            }
            (None, b'>') if peek(bytes, i + 1) == Some(b'(') => {
                return Err(ShellParseError::Unparseable);
            }
            (None, b'>') | (None, b'<') => return Err(ShellParseError::Unparseable),
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

fn peek(bytes: &[u8], idx: usize) -> Option<u8> {
    bytes.get(idx).copied()
}

fn split_top_level(command: &str) -> Result<Vec<String>, ShellParseError> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match (quote, b) {
            (Some(q), c) if c == q => {
                quote = None;
                current.push(b as char);
                i += 1;
            }
            (Some(b'\''), _) => {
                current.push(b as char);
                i += 1;
            }
            (Some(_), b'\\') if i + 1 < bytes.len() => {
                current.push(b as char);
                current.push(bytes[i + 1] as char);
                i += 2;
            }
            (None, b'\'') | (None, b'"') => {
                quote = Some(b);
                current.push(b as char);
                i += 1;
            }
            (None, b'&') if peek(bytes, i + 1) == Some(b'&') => {
                push_segment(&mut segments, &mut current);
                i += 2;
            }
            (None, b'|') if peek(bytes, i + 1) == Some(b'|') => {
                push_segment(&mut segments, &mut current);
                i += 2;
            }
            (None, b';') | (None, b'|') => {
                push_segment(&mut segments, &mut current);
                i += 1;
            }
            (None, b'&') => {
                // Background operator — we can't statically reason about it.
                return Err(ShellParseError::Unparseable);
            }
            _ => {
                current.push(b as char);
                i += 1;
            }
        }
    }
    if quote.is_some() {
        return Err(ShellParseError::Unparseable);
    }
    push_segment(&mut segments, &mut current);
    Ok(segments)
}

fn push_segment(segments: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        segments.push(trimmed.to_string());
    }
    current.clear();
}

fn tokenize(segment: &str) -> Result<Vec<String>, ShellParseError> {
    let bytes = segment.as_bytes();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match (quote, b) {
            (Some(q), c) if c == q => {
                quote = None;
                in_token = true;
                i += 1;
            }
            (Some(b'\''), _) => {
                current.push(b as char);
                i += 1;
            }
            (Some(_), b'\\') if i + 1 < bytes.len() => {
                current.push(bytes[i + 1] as char);
                i += 2;
            }
            (None, b'\'') | (None, b'"') => {
                quote = Some(b);
                in_token = true;
                i += 1;
            }
            (None, b'\\') if i + 1 < bytes.len() => {
                current.push(bytes[i + 1] as char);
                in_token = true;
                i += 2;
            }
            (None, b' ') | (None, b'\t') | (None, b'\n') => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
                i += 1;
            }
            _ => {
                current.push(b as char);
                in_token = true;
                i += 1;
            }
        }
    }
    if quote.is_some() {
        return Err(ShellParseError::Unparseable);
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<Vec<String>> {
        parse_command_pipeline(s).expect("expected to parse")
    }

    #[test]
    fn empty_returns_empty() {
        assert_eq!(parse_command_pipeline("").unwrap(), Vec::<Vec<String>>::new());
        assert_eq!(parse_command_pipeline("   ").unwrap(), Vec::<Vec<String>>::new());
    }

    #[test]
    fn simple_command_one_argv() {
        assert_eq!(parse("ls"), vec![vec!["ls".to_string()]]);
    }

    #[test]
    fn command_with_args() {
        assert_eq!(
            parse("ls -la /tmp"),
            vec![vec!["ls".to_string(), "-la".into(), "/tmp".into()]]
        );
    }

    #[test]
    fn splits_on_double_ampersand() {
        assert_eq!(
            parse("ls && cat foo"),
            vec![vec!["ls".to_string()], vec!["cat".into(), "foo".into()]]
        );
    }

    #[test]
    fn splits_on_double_pipe() {
        assert_eq!(
            parse("ls || true"),
            vec![vec!["ls".to_string()], vec!["true".into()]]
        );
    }

    #[test]
    fn splits_on_semicolon() {
        assert_eq!(
            parse("ls ; cat foo"),
            vec![vec!["ls".to_string()], vec!["cat".into(), "foo".into()]]
        );
    }

    #[test]
    fn splits_on_pipe() {
        assert_eq!(
            parse("ls | grep foo"),
            vec![
                vec!["ls".to_string()],
                vec!["grep".into(), "foo".into()],
            ]
        );
    }

    #[test]
    fn splits_compound_pipeline() {
        assert_eq!(
            parse("a | b ; c"),
            vec![
                vec!["a".to_string()],
                vec!["b".into()],
                vec!["c".into()],
            ]
        );
    }

    #[test]
    fn quoted_arguments_preserved() {
        assert_eq!(
            parse("echo \"hello world\""),
            vec![vec!["echo".to_string(), "hello world".into()]]
        );
    }

    #[test]
    fn single_quoted_arguments_preserved() {
        assert_eq!(
            parse("grep 'foo bar' file.txt"),
            vec![vec![
                "grep".to_string(),
                "foo bar".into(),
                "file.txt".into()
            ]]
        );
    }

    #[test]
    fn operator_inside_quotes_does_not_split() {
        assert_eq!(
            parse("echo \"a && b\""),
            vec![vec!["echo".to_string(), "a && b".into()]]
        );
    }

    #[test]
    fn backticks_rejected() {
        assert_eq!(
            parse_command_pipeline("echo `whoami`"),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn command_substitution_rejected() {
        assert_eq!(
            parse_command_pipeline("echo $(whoami)"),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn command_substitution_inside_double_quotes_rejected() {
        // bash expands `$(...)` inside `"..."` — the inner command runs.
        // Treating the outer string as plain text would let the agent
        // hide a destructive command behind a safelisted echo.
        assert_eq!(
            parse_command_pipeline("echo \"$(rm -rf build)\""),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn backticks_inside_double_quotes_rejected() {
        assert_eq!(
            parse_command_pipeline("echo \"`whoami`\""),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn command_substitution_inside_single_quotes_is_literal() {
        // Single quotes are literal in bash; `$(...)` inside is just text.
        assert_eq!(
            parse("echo '$(rm -rf build)'"),
            vec![vec!["echo".to_string(), "$(rm -rf build)".into()]]
        );
    }

    #[test]
    fn process_substitution_rejected() {
        assert_eq!(
            parse_command_pipeline("diff <(ls) <(ls -a)"),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn redirection_rejected() {
        assert_eq!(
            parse_command_pipeline("echo hi > file"),
            Err(ShellParseError::Unparseable)
        );
        assert_eq!(
            parse_command_pipeline("cat < file"),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn background_operator_rejected() {
        assert_eq!(
            parse_command_pipeline("sleep 100 &"),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn unterminated_quote_rejected() {
        assert_eq!(
            parse_command_pipeline("echo \"oops"),
            Err(ShellParseError::Unparseable)
        );
    }

    #[test]
    fn rm_rf_root_parsed_as_argv() {
        // Parsing alone doesn't classify danger — that's the classifier's job.
        assert_eq!(
            parse("rm -rf /"),
            vec![vec!["rm".to_string(), "-rf".into(), "/".into()]]
        );
    }

    #[test]
    fn empty_segments_skipped() {
        assert_eq!(parse("ls ;"), vec![vec!["ls".to_string()]]);
        assert_eq!(parse("; ls"), vec![vec!["ls".to_string()]]);
    }
}
