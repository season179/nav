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

    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] as char {
            '*' => {
                let double = i + 1 < bytes.len() && bytes[i + 1] == b'*';
                if double {
                    // `**/` matches any number of leading path segments
                    // (including none); a bare `**` matches anything.
                    if i + 2 < bytes.len() && bytes[i + 2] == b'/' {
                        re.push_str("(?:.*/)?");
                        i += 3;
                    } else {
                        re.push_str(".*");
                        i += 2;
                    }
                } else {
                    re.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                re.push_str("[^/]");
                i += 1;
            }
            other => {
                // Escape regex metacharacters; pass everything else literally.
                if "\\.+()|[]{}^$".contains(other) {
                    re.push('\\');
                }
                re.push(other);
                i += 1;
            }
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
}
