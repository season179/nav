//! Minimal glob support, implemented by translating a glob to an anchored
//! regex so `find` and `grep` share one matcher (and the single `regex` dep).
//!
//! Supported: `*` (any run except `/`), `**` (any run including `/`), `?` (one
//! non-`/` char), and literal text. Paths are matched with `/` separators.

use regex::Regex;

use super::ToolError;

/// Compile a glob pattern into an anchored regex over `/`-separated paths.
pub fn glob_to_regex(pattern: &str) -> Result<Regex, ToolError> {
    let mut re = String::with_capacity(pattern.len() * 2 + 2);
    re.push('^');

    // Iterate by Unicode scalar so multi-byte literals stay intact.
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    // `**/` matches any number of leading path segments
                    // (including none); a bare `**` matches anything.
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        re.push_str("(?:.*/)?");
                    } else {
                        re.push_str(".*");
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            // Escape regex metacharacters; pass everything else literally.
            other => re.push_str(&regex::escape(&other.to_string())),
        }
    }

    re.push('$');
    Regex::new(&re).map_err(|error| ToolError::new(format!("invalid glob {pattern:?}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        glob_to_regex(pattern).unwrap().is_match(path)
    }

    #[test]
    fn star_does_not_cross_directories() {
        assert!(matches("*.rs", "main.rs"));
        assert!(!matches("*.rs", "src/main.rs"));
    }

    #[test]
    fn double_star_crosses_directories() {
        assert!(matches("**/*.rs", "src/a/b/main.rs"));
        assert!(matches("**/*.rs", "main.rs"));
        assert!(matches("src/**/*.json", "src/a/b.json"));
    }

    #[test]
    fn question_matches_one_non_separator() {
        assert!(matches("a?.txt", "ab.txt"));
        assert!(!matches("a?.txt", "a/b.txt"));
    }

    #[test]
    fn literal_dots_are_escaped() {
        assert!(matches("a.txt", "a.txt"));
        assert!(!matches("a.txt", "axtxt"));
    }

    #[test]
    fn multibyte_literals_match() {
        // Char-wise iteration must keep multi-byte literals intact.
        assert!(matches("café/*.rs", "café/main.rs"));
        assert!(matches("**/票据.txt", "a/b/票据.txt"));
        assert!(!matches("café/*.rs", "cafe/main.rs"));
    }
}
