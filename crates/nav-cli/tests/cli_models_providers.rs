//! End-to-end coverage for `nav models list` and `nav providers list`. The
//! binary is invoked as a subprocess against a fixture `.nav/settings.json`
//! catalog so the parser, settings load, and renderer all run together.

use std::process::Command;

const FIXTURE_SETTINGS: &str = r#"{
    "providers": {
        "z.ai": {
            "name": "Z.AI",
            "base_url": "https://api.z.ai/v1",
            "models": {
                "glm-5.1": {
                    "reasoning_effort": "high"
                }
            }
        },
        "ollama": {
            "base_url": "http://localhost:11434/v1",
            "models": {
                "llama3": {}
            }
        }
    }
}"#;

/// Lay out a workspace with `.nav/settings.json` and a separate `HOME` so
/// the test never reads the developer's real `~/.nav/`.
fn write_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let nav_dir = workspace.path().join(".nav");
    std::fs::create_dir_all(&nav_dir).unwrap();
    std::fs::write(nav_dir.join("settings.json"), FIXTURE_SETTINGS).unwrap();
    (workspace, home)
}

fn run_nav(args: &[&str], cwd: &std::path::Path, home: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nav"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .output()
        .expect("invoke nav binary")
}

#[test]
fn models_list_text_output_includes_provider_display_and_reasoning() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["models", "list"], workspace.path(), home.path());
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    // BTreeMap orders providers lexicographically: ollama before z.ai.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "stdout: {stdout}");
    // The provider with `name` unset falls back to its id — assert the
    // exact column shape so a regression that collapses the display column
    // to an empty string can't pass.
    assert_eq!(lines[0], "ollama/llama3  ollama");
    assert!(lines[1].starts_with("z.ai/glm-5.1  Z.AI"), "got: {}", lines[1]);
    assert!(lines[1].contains("reasoning=high"));
}

#[test]
fn models_list_json_output_is_array() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["models", "list", "--json"], workspace.path(), home.path());
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["selector"], "ollama/llama3");
    assert_eq!(arr[1]["selector"], "z.ai/glm-5.1");
    assert_eq!(arr[1]["reasoning_effort"], "high");
    // None fields skip-serialize so consumers can use `has(...)` to
    // discriminate unset from set.
    assert!(arr[0].get("reasoning_effort").is_none(), "expected reasoning_effort absent for ollama, got {:?}", arr[0]);
    assert!(arr[0].get("model_id").is_none());
}

#[test]
fn providers_list_text_output_reports_credential_state() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["providers", "list"], workspace.path(), home.path());
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "stdout: {stdout}");
    // Fixture omits `api_key` for both providers — `n/a` is the only value
    // the renderer can produce. The display-name column for ollama is the
    // fallback-to-id; we assert the exact text so a regression that emits
    // an empty column can't pass.
    assert_eq!(
        lines[0],
        "ollama  ollama  http://localhost:11434/v1  credential resolvable: n/a"
    );
    assert_eq!(
        lines[1],
        "z.ai  Z.AI  https://api.z.ai/v1  credential resolvable: n/a"
    );
}

#[test]
fn providers_list_json_output_is_array() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["providers", "list", "--json"], workspace.path(), home.path());
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], "ollama");
    assert_eq!(arr[0]["display_name"], "ollama");
    assert_eq!(arr[0]["credential_configured"], false);
    assert_eq!(arr[0]["credential_resolvable"], true);
    assert_eq!(arr[1]["id"], "z.ai");
    assert_eq!(arr[1]["display_name"], "Z.AI");
}

#[test]
fn broken_pipe_does_not_panic() {
    // Pipe `nav models list` through `true`, which closes stdout
    // immediately. Without the BrokenPipe-safe writer the binary panics on
    // the next `println!` and prints a `panicked at` line to stderr.
    let (workspace, home) = write_fixture();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{:?} models list | true",
            env!("CARGO_BIN_EXE_nav")
        ))
        .current_dir(workspace.path())
        .env("HOME", home.path())
        .output()
        .expect("invoke nav through pipe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "expected no panic; stderr was: {stderr}"
    );
}

#[test]
fn empty_catalog_prints_placeholder() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // No .nav/settings.json — both commands report no entries instead of
    // crashing or talking to the network.
    let out = run_nav(&["models", "list"], workspace.path(), home.path());
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("(no models configured)"), "stdout: {stdout}");

    let out = run_nav(&["providers", "list"], workspace.path(), home.path());
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("(no providers configured)"), "stdout: {stdout}");
}
