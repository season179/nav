//! The file sink must stay bounded: writing far more than the cap leaves only
//! ~10 MiB on disk (oldest segments dropped) while retaining the newest lines.

use std::io::Write;
use std::path::Path;

use nav::logging::{SEGMENT_BYTES, SEGMENTS, build_file_rotate};
use uuid::Uuid;

/// Sum the bytes of the active log plus every rotated segment beside it.
fn total_log_bytes(path: &Path) -> u64 {
    let dir = path.parent().expect("log path has a parent");
    let prefix = path.file_name().expect("log path has a file name");
    std::fs::read_dir(dir)
        .expect("log directory is readable")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&*prefix.to_string_lossy())
        })
        .map(|entry| entry.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

#[test]
fn file_sink_stays_within_the_size_cap_and_keeps_newest_lines() {
    let dir = std::env::temp_dir().join(format!("nav-log-{}", Uuid::now_v7()));
    let path = dir.join("nav.log");

    let mut writer = build_file_rotate(&path).expect("writer builds");

    // Write ~12 MiB — comfortably past the SEGMENTS * SEGMENT_BYTES (~10 MiB) cap.
    let line = format!("{}\n", "x".repeat(255));
    let target = (SEGMENTS * SEGMENT_BYTES) + (2 * SEGMENT_BYTES);
    let mut written = 0usize;
    while written < target {
        writer.write_all(line.as_bytes()).expect("write succeeds");
        written += line.len();
    }
    let marker = format!("MARKER-{}\n", Uuid::now_v7());
    writer.write_all(marker.as_bytes()).expect("marker write");
    writer.flush().expect("flush succeeds");

    // Capped: rotation discarded the oldest segments rather than growing forever.
    let total = total_log_bytes(&path);
    let cap = (SEGMENTS * SEGMENT_BYTES) as u64;
    assert!(
        total <= cap,
        "log footprint {total} bytes exceeds cap {cap} bytes",
    );
    // And it genuinely rotated (more than a single segment's worth survived).
    assert!(
        total > SEGMENT_BYTES as u64,
        "expected multiple retained segments, only {total} bytes on disk",
    );

    // Newest content survives: the marker sits in the active file.
    let active = std::fs::read_to_string(&path).expect("active log readable");
    assert!(
        active.contains(marker.trim_end()),
        "newest line was dropped"
    );

    std::fs::remove_dir_all(&dir).ok();
}
