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

const PROVIDER_API_KEY_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "GROQ_API_KEY",
    "TOGETHER_API_KEY",
    "ZAI_API_KEY",
];

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
    let mut command = Command::new(env!("CARGO_BIN_EXE_nav"));
    command.args(args).current_dir(cwd).env("HOME", home);
    for key in PROVIDER_API_KEY_ENV {
        command.env_remove(key);
    }
    command.output().expect("invoke nav binary")
}

#[test]
fn models_list_text_output_includes_provider_display_and_reasoning() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["models", "list"], workspace.path(), home.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    // BTreeMap orders providers lexicographically: ollama before z.ai.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "stdout: {stdout}");
    // The provider with `name` unset falls back to its id — assert the
    // exact column shape so a regression that collapses the display column
    // to an empty string can't pass.
    assert_eq!(lines[0], "ollama/llama3  ollama");
    assert!(
        lines[1].starts_with("z.ai/glm-5.1  Z.AI"),
        "got: {}",
        lines[1]
    );
    assert!(lines[1].contains("reasoning=high"));
}

#[test]
fn models_list_json_output_is_array() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["models", "list", "--json"], workspace.path(), home.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["selector"], "ollama/llama3");
    assert_eq!(arr[1]["selector"], "z.ai/glm-5.1");
    assert_eq!(arr[1]["reasoning_effort"], "high");
    // None fields skip-serialize so consumers can use `has(...)` to
    // discriminate unset from set.
    assert!(
        arr[0].get("reasoning_effort").is_none(),
        "expected reasoning_effort absent for ollama, got {:?}",
        arr[0]
    );
    assert!(arr[0].get("model_id").is_none());
}

#[test]
fn providers_list_text_output_reports_credential_state() {
    let (workspace, home) = write_fixture();
    let out = run_nav(&["providers", "list"], workspace.path(), home.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 9, "stdout: {stdout}");
    // Built-ins are always present; project providers with the same id replace
    // the built-in entry. The fixture's `ollama` has no display name and no
    // api_key, so it should render with fallback display text and `n/a`.
    assert!(
        lines.contains(&"ollama  ollama  http://localhost:11434/v1  credential resolvable: n/a")
    );
    assert!(lines.contains(&"z.ai  Z.AI  https://api.z.ai/v1  credential resolvable: n/a"));
    assert!(
        lines.contains(&"openai  OpenAI  https://api.openai.com/v1  credential resolvable: yes")
    );
}

#[test]
fn providers_list_json_output_is_array() {
    let (workspace, home) = write_fixture();
    let out = run_nav(
        &["providers", "list", "--json"],
        workspace.path(),
        home.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 9);
    let by_id = |id: &str| {
        arr.iter()
            .find(|entry| entry["id"] == id)
            .unwrap_or_else(|| panic!("missing provider {id}: {arr:#?}"))
    };
    let ollama = by_id("ollama");
    assert_eq!(ollama["display_name"], "ollama");
    assert_eq!(ollama["credential_configured"], false);
    assert_eq!(ollama["credential_resolvable"], true);
    let zai_custom = by_id("z.ai");
    assert_eq!(zai_custom["display_name"], "Z.AI");
    let openai = by_id("openai");
    assert_eq!(openai["credential_configured"], true);
    assert_eq!(openai["credential_resolvable"], true);
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
    // No .nav/settings.json — models are empty, while providers still show
    // the built-in provider catalog.
    let out = run_nav(&["models", "list"], workspace.path(), home.path());
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("(no models configured)"),
        "stdout: {stdout}"
    );

    let out = run_nav(&["providers", "list"], workspace.path(), home.path());
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("openai  OpenAI"), "stdout: {stdout}");
    assert!(
        stdout.contains("ollama  Ollama (local)"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("(no providers configured)"),
        "stdout: {stdout}"
    );
}
