//! Protected-path rules.
//!
//! Two lists:
//! - `PROTECTED_METADATA_NAMES` — directory names whose contents must not be
//!   written even inside a writable root (`.git`, `.agents`, `.nav`).
//! - `PROTECTED_READ_GLOBS` — file-name globs that hold secrets and require
//!   approval even for reads (`.env`, keys, etc.).
//!
//! Mirrors codex's `PROTECTED_METADATA_PATH_NAMES` semantics.

use std::path::Path;

pub const PROTECTED_METADATA_NAMES: &[&str] = &[".git", ".agents", ".nav"];

/// File-name globs that mark protected reads. Matched against the final
/// path component only (so `src/.envoy.rs` does not match `.env*`).
pub const PROTECTED_READ_GLOBS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa",
    "id_rsa.pub",
    "id_ed25519",
    "id_ed25519.pub",
];

/// True if any component of `path` matches a protected-metadata directory
/// name. Used to block writes to `.git/HEAD`, `.agents/skills/x.md`, etc.
pub fn is_protected_metadata_write<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().components().any(|c| {
        let bytes = c.as_os_str().as_encoded_bytes();
        PROTECTED_METADATA_NAMES
            .iter()
            .any(|p| p.as_bytes() == bytes)
    })
}

/// True if the file name (last component) of `path` matches a protected-read
/// glob. Reading these files requires approval even when the path is in-tree.
pub fn is_protected_read<P: AsRef<Path>>(path: P) -> bool {
    let Some(name) = path
        .as_ref()
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
    else {
        return false;
    };
    PROTECTED_READ_GLOBS.iter().any(|g| glob_matches(g, &name))
}

/// Minimal glob matcher: supports `*` as a single wildcard segment. Good
/// enough for the static patterns in `PROTECTED_READ_GLOBS`; if the list
/// grows beyond simple cases we can pull in `globset`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pat = pattern.as_bytes();
    let s = name.as_bytes();
    glob_helper(pat, s)
}

fn glob_helper(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(b'*'), _) => {
            // Try matching the rest with zero or more characters consumed.
            if glob_helper(&pat[1..], s) {
                return true;
            }
            if !s.is_empty() {
                return glob_helper(pat, &s[1..]);
            }
            false
        }
        (Some(_), None) => false,
        (Some(a), Some(b)) if a == b => glob_helper(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// True if the glob pattern `pattern` *could* expand to the literal `target`.
/// Supports `*`, `?`, and `[...]` bracket expressions (over-approximated as
/// "any single char"). Used to flag args like `.gi?` that the shell expands
/// to `.git` before the classifier sees the final argv.
pub fn glob_could_match(pattern: &str, target: &str) -> bool {
    glob_could_match_helper(pattern.as_bytes(), target.as_bytes())
}

fn glob_could_match_helper(pat: &[u8], s: &[u8]) -> bool {
    if pat.is_empty() {
        return s.is_empty();
    }
    match pat[0] {
        b'*' => {
            if glob_could_match_helper(&pat[1..], s) {
                return true;
            }
            for i in 1..=s.len() {
                if glob_could_match_helper(&pat[1..], &s[i..]) {
                    return true;
                }
            }
            false
        }
        b'?' => !s.is_empty() && glob_could_match_helper(&pat[1..], &s[1..]),
        b'[' => {
            // Over-approximate bracket as "match any single char": if the
            // pattern says any-of-set, the worst case is the set includes
            // whatever's in `s` at this position.
            if let Some(end) = pat.iter().position(|&b| b == b']') {
                !s.is_empty() && glob_could_match_helper(&pat[end + 1..], &s[1..])
            } else {
                // Unclosed bracket — be conservative, accept.
                true
            }
        }
        c => !s.is_empty() && s[0] == c && glob_could_match_helper(&pat[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_directory_is_protected() {
        assert!(is_protected_metadata_write("foo/.git/HEAD"));
        assert!(is_protected_metadata_write(".git/config"));
    }

    #[test]
    fn agents_directory_is_protected() {
        assert!(is_protected_metadata_write(".agents/skills/x.md"));
        assert!(is_protected_metadata_write("sub/.agents/foo"));
    }

    #[test]
    fn nav_directory_is_protected() {
        assert!(is_protected_metadata_write(".nav/sessions/db"));
    }

    #[test]
    fn similar_named_files_are_not_protected() {
        assert!(!is_protected_metadata_write("docs/git-guide.md"));
        assert!(!is_protected_metadata_write("src/agents.rs"));
        assert!(!is_protected_metadata_write("nav.toml"));
    }

    #[test]
    fn env_file_is_protected_read() {
        assert!(is_protected_read(".env"));
        assert!(is_protected_read(".env.local"));
        assert!(is_protected_read(".env.production"));
        assert!(is_protected_read("subdir/.env"));
    }

    #[test]
    fn env_suffix_does_not_falsely_match() {
        assert!(!is_protected_read("src/.envoy.rs"));
        assert!(!is_protected_read("envelope.txt"));
        assert!(!is_protected_read(".environment.json"));
    }

    #[test]
    fn pem_and_key_are_protected_read() {
        assert!(is_protected_read("server.pem"));
        assert!(is_protected_read("private.key"));
        assert!(is_protected_read("certs/api.pem"));
    }

    #[test]
    fn ssh_keys_are_protected_read() {
        assert!(is_protected_read("foo/id_rsa"));
        assert!(is_protected_read("id_rsa.pub"));
        assert!(is_protected_read("home/id_ed25519"));
    }

    #[test]
    fn ordinary_files_are_not_protected_read() {
        assert!(!is_protected_read("README.md"));
        assert!(!is_protected_read("src/main.rs"));
        assert!(!is_protected_read("env.txt"));
    }

    #[test]
    fn glob_star_handles_prefix_match() {
        assert!(glob_matches("foo*", "foo"));
        assert!(glob_matches("foo*", "foobar"));
        assert!(!glob_matches("foo*", "fo"));
    }

    #[test]
    fn glob_star_handles_dot_prefix() {
        assert!(glob_matches(".env.*", ".env.local"));
        assert!(!glob_matches(".env.*", ".env"));
        assert!(!glob_matches(".env.*", "env.local"));
    }
}
