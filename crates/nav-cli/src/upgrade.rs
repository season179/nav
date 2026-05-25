//! GitHub-release-based self-update for `nav update` / `nav upgrade`.
//!
//! Replaces the older `cargo install --path` flow so users installed via
//! `curl | bash` (see `install.sh`) can update without a Rust toolchain or
//! a repo checkout. The pre-release flow lives in `install.sh`; this module
//! mirrors it in Rust so an already-running `nav` can replace itself.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

const REPO: &str = "season179/nav";
const TARGET: &str = "aarch64-apple-darwin";
const TARGET_OS: &str = "macos";
const TARGET_ARCH: &str = "aarch64";

/// Entry point used by `main`. Resolves the running binary, asks GitHub for
/// the latest release, verifies the tarball, and swaps it in.
pub async fn run() -> Result<()> {
    // Fail fast on platforms with no published binary so the user sees a
    // clear "self-update not supported here" instead of an opaque 404 from
    // the asset download a few steps later.
    if std::env::consts::OS != TARGET_OS || std::env::consts::ARCH != TARGET_ARCH {
        bail!(
            "nav update: self-update only supports {TARGET} (this machine is {}-{}).\n\
             Build from source: `cargo install --path crates/nav-cli --force`\n\
             Or browse releases: https://github.com/{REPO}/releases",
            std::env::consts::ARCH,
            std::env::consts::OS,
        );
    }

    let current_exe = std::env::current_exe()
        .context("could not determine the path of the running nav binary")?;
    let current_version = env!("CARGO_PKG_VERSION");

    let latest = match fetch_latest_version().await {
        Ok(v) => v,
        Err(err) => {
            bail!(
                "nav update: could not check for updates ({err}).\n\
                 Visit https://github.com/{REPO}/releases to download manually."
            );
        }
    };

    // Numeric compare so a dev build that's CalVer-ahead of the latest
    // release never gets silently downgraded.
    match compare_versions(&latest, current_version) {
        Some(Ordering::Equal) => {
            println!("nav is already at the latest version ({latest}).");
            return Ok(());
        }
        Some(Ordering::Less) => {
            println!(
                "nav is at {current_version}, newer than the latest release ({latest}). Nothing to do."
            );
            return Ok(());
        }
        // Newer remote OR unparseable version on either side: proceed. An
        // unparseable tag (e.g. a pre-release like `v26.5.8-rc.1`) shouldn't
        // be cataloged as "latest" by GitHub for stable users, but if it is
        // we still let the swap happen so the user can override explicitly.
        _ => {}
    }

    let tmp = tempfile::tempdir().context("could not create a temporary directory")?;
    let tarball_name = format!("nav-{latest}-{TARGET}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/v{latest}");
    let tarball_path = tmp.path().join(&tarball_name);
    let checksum_path = tmp.path().join(format!("{tarball_name}.sha256"));

    println!("nav update: downloading {tarball_name}…");
    download(&format!("{base}/{tarball_name}"), &tarball_path)
        .await
        .with_context(|| {
            format!(
                "nav update: failed to download release asset.\n\
                 Visit https://github.com/{REPO}/releases to download manually."
            )
        })?;
    download(&format!("{base}/{tarball_name}.sha256"), &checksum_path)
        .await
        .context("nav update: failed to download checksum file")?;

    verify_checksum(&tarball_path, &checksum_path)?;
    println!("nav update: checksum verified.");

    extract_tarball(&tarball_path, tmp.path())?;
    let new_binary = find_extracted_nav(tmp.path())
        .context("nav update: could not find the nav binary in the downloaded archive")?;

    replace_binary(&current_exe, &new_binary)?;

    let suffix = if is_under_cargo_bin(&current_exe) {
        "  (replaced via ~/.cargo/bin/nav)"
    } else {
        ""
    };
    println!("nav updated: {current_version} → {latest}{suffix}");
    Ok(())
}

/// Fetch the latest release tag and strip the leading `v`. Returns just the
/// bare version (e.g. `26.5.8`) so callers can compare against
/// `env!("CARGO_PKG_VERSION")` directly. Maps a GitHub 403 to a clear
/// "rate limited" reason so the wrapper can render the manual-download hint.
async fn fetch_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = http_client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("could not reach the GitHub Releases API")?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        bail!("rate limited");
    }
    if !status.is_success() {
        bail!("GitHub API returned {status}");
    }
    let body: serde_json::Value = response
        .json()
        .await
        .context("could not parse GitHub Releases API response")?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .context("GitHub release payload is missing a tag_name")?;
    Ok(tag.trim_start_matches('v').to_string())
}

async fn download(url: &str, dest: &Path) -> Result<()> {
    let response = http_client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?;
    if !response.status().is_success() {
        bail!("{url} returned {}", response.status());
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("could not read response from {url}"))?;
    std::fs::write(dest, &bytes).with_context(|| format!("could not write {}", dest.display()))?;
    Ok(())
}

fn http_client() -> Result<reqwest::Client> {
    // GitHub's API rejects clients without a User-Agent, so set one explicitly.
    reqwest::Client::builder()
        .user_agent(concat!("nav/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not build HTTP client")
}

fn verify_checksum(tarball: &Path, checksum_file: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(checksum_file)
        .with_context(|| format!("could not read {}", checksum_file.display()))?;
    let expected = parse_checksum_line(&raw)
        .with_context(|| format!("checksum file is empty: {}", checksum_file.display()))?;
    let actual = hash_file(tarball)?;
    if actual != expected {
        bail!(
            "nav update: checksum verification failed — aborting.\n  expected: {expected}\n  actual:   {actual}"
        );
    }
    Ok(())
}

/// Read the first whitespace-separated token of a `shasum -a 256` output line
/// and normalise to lowercase hex.
fn parse_checksum_line(raw: &str) -> Option<String> {
    raw.split_whitespace().next().map(|s| s.to_lowercase())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut file =
        std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("could not hash {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Shell out to the system `tar` rather than pulling in `flate2`/`tar` crates:
/// `install.sh` already requires `tar`, so demanding it here keeps the
/// install-time and update-time toolchains consistent.
fn extract_tarball(tarball: &Path, into: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(into)
        .status()
        .context("failed to invoke `tar` (is it on PATH?)")?;
    if !status.success() {
        bail!("tar extraction exited with {status}");
    }
    Ok(())
}

/// Locate the extracted `nav` binary. The release tarball ships a flat layout
/// today, but tolerate one level of nesting so future packaging changes don't
/// silently break self-update.
fn find_extracted_nav(dir: &Path) -> Result<PathBuf> {
    let direct = dir.join("nav");
    if direct.is_file() {
        return Ok(direct);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let nested = path.join("nav");
            if nested.is_file() {
                return Ok(nested);
            }
        }
    }
    bail!(
        "nav binary not found in extracted archive at {}",
        dir.display()
    );
}

/// Atomically swap the running binary for the freshly downloaded one.
///
/// Sequence (Unix-only):
/// 1. Stage the new binary as a sibling of `current_exe` so the final
///    `rename` stays on the same filesystem — the tarball lives in `$TMPDIR`,
///    which can be a different mount than `~/.nav/bin`.
/// 2. `chmod +x` the staged file.
/// 3. Rename the running binary to `<current_exe>.old`. On Unix this leaves
///    the live process running against the original inode.
/// 4. Rename the staged binary into place. This is the visible "swap".
/// 5. Best-effort delete the `.old` backup. If step 4 fails we restore the
///    backup so the user is never left without `nav`.
fn replace_binary(current_exe: &Path, new_binary: &Path) -> Result<()> {
    let staged = sibling_with_suffix(current_exe, ".new");
    let backup = sibling_with_suffix(current_exe, ".old");

    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_file(&backup);

    std::fs::copy(new_binary, &staged).with_context(|| replace_error_message(current_exe))?;
    set_executable(&staged).with_context(|| replace_error_message(current_exe))?;

    std::fs::rename(current_exe, &backup).with_context(|| replace_error_message(current_exe))?;
    if let Err(err) = std::fs::rename(&staged, current_exe) {
        // Restore the previous binary so the user isn't stranded.
        let _ = std::fs::rename(&backup, current_exe);
        let _ = std::fs::remove_file(&staged);
        return Err(err).with_context(|| replace_error_message(current_exe));
    }

    let _ = std::fs::remove_file(&backup);
    Ok(())
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(suffix);
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("could not stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("could not chmod {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn replace_error_message(current_exe: &Path) -> String {
    // Deliberately do NOT suggest `sudo nav update`: a sudo'd swap leaves the
    // binary root-owned, which then makes every subsequent non-sudo update
    // fail in the same way with no obvious cause. Steering the user toward
    // the install script (which writes into `~/.nav/bin`) avoids that trap.
    format!(
        "nav update: could not replace the binary at {}\n\
         The binary's directory is not user-writable, so self-update can't proceed.\n\
         Reinstall to a user-writable location: \
         curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | bash",
        current_exe.display()
    )
}

/// Numeric ordering on dot-separated CalVer components (`26.5.8` etc.).
/// Returns `None` if either side has a non-numeric component, so callers
/// can fall back to a conservative strategy (e.g. proceed but never
/// downgrade-by-assumption).
fn compare_versions(latest: &str, current: &str) -> Option<Ordering> {
    let parse =
        |v: &str| -> Option<Vec<u64>> { v.split('.').map(|s| s.parse::<u64>().ok()).collect() };
    Some(parse(latest)?.cmp(&parse(current)?))
}

fn is_under_cargo_bin(path: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    path.starts_with(home.join(".cargo").join("bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_line_picks_first_token_lowercase() {
        let parsed = parse_checksum_line("ABCDEF  nav-1.0.0.tar.gz\n").unwrap();
        assert_eq!(parsed, "abcdef");
    }

    #[test]
    fn checksum_line_empty_returns_none() {
        assert!(parse_checksum_line("").is_none());
        assert!(parse_checksum_line("   \n").is_none());
    }

    #[test]
    fn sibling_with_suffix_appends_to_basename() {
        let p = PathBuf::from("/usr/local/bin/nav");
        assert_eq!(
            sibling_with_suffix(&p, ".old"),
            PathBuf::from("/usr/local/bin/nav.old")
        );
    }

    #[test]
    fn find_extracted_nav_prefers_direct_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nav"), b"#!/bin/sh\n").unwrap();
        let found = find_extracted_nav(dir.path()).unwrap();
        assert_eq!(found, dir.path().join("nav"));
    }

    #[test]
    fn find_extracted_nav_searches_one_level_of_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nav-1.0.0-aarch64-apple-darwin");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("nav"), b"#!/bin/sh\n").unwrap();
        let found = find_extracted_nav(dir.path()).unwrap();
        assert_eq!(found, sub.join("nav"));
    }

    #[test]
    fn find_extracted_nav_errors_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_extracted_nav(dir.path()).is_err());
    }

    #[test]
    fn hash_file_matches_known_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, b"hello\n").unwrap();
        // `echo hello | shasum -a 256`
        assert_eq!(
            hash_file(&path).unwrap(),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn verify_checksum_rejects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("nav.tar.gz");
        std::fs::write(&tarball, b"hello\n").unwrap();
        let checksum = dir.path().join("nav.tar.gz.sha256");
        std::fs::write(&checksum, "deadbeef  nav.tar.gz\n").unwrap();
        let err = verify_checksum(&tarball, &checksum).unwrap_err();
        assert!(err.to_string().contains("checksum verification failed"));
    }

    #[test]
    fn compare_versions_orders_calver_numerically() {
        assert_eq!(
            compare_versions("26.5.8", "26.5.7"),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_versions("26.5.7", "26.5.8"), Some(Ordering::Less));
        assert_eq!(compare_versions("26.5.7", "26.5.7"), Some(Ordering::Equal));
        // Numeric, not lexicographic: 10 > 9.
        assert_eq!(
            compare_versions("26.10.0", "26.9.99"),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn compare_versions_returns_none_for_unparseable() {
        assert!(compare_versions("26.5.8-rc.1", "26.5.7").is_none());
        assert!(compare_versions("26.5.7", "unknown").is_none());
        assert!(compare_versions("", "26.5.7").is_none());
    }

    #[test]
    fn verify_checksum_accepts_match() {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("nav.tar.gz");
        std::fs::write(&tarball, b"hello\n").unwrap();
        let digest = hash_file(&tarball).unwrap();
        let checksum = dir.path().join("nav.tar.gz.sha256");
        std::fs::write(&checksum, format!("{digest}  nav.tar.gz\n")).unwrap();
        verify_checksum(&tarball, &checksum).unwrap();
    }
}
