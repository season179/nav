use std::path::PathBuf;
use std::process::Command;

use nav_harness::models::{DecodedPart, DecodedProviderPayload, DecodedTurn};
use nav_harness::sessions::{
    CreateSession, DecodeStatus, NewProviderPayload, Part, ProviderPayloadDirection, RunStatus,
    SqliteSessionStore, StartRun, Turn, TurnMeta, TurnRole,
};
use nav_types::{MessageId, RunId, SessionId};

struct TempDataDir {
    path: PathBuf,
}

impl TempDataDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "nav-backend-recover-payloads-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir(&path).expect("temp data dir should be created");
        Self { path }
    }

    fn db_path(&self) -> PathBuf {
        self.path.join("nav.db")
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn recover_payloads_reports_diff_for_older_decoder_projection() {
    let data_dir = TempDataDir::new("diff-report");
    let db_path = data_dir.db_path();
    let store = SqliteSessionStore::open(&db_path).expect("store should open");
    let session_id = SessionId::new_unchecked("019e7000-0000-7000-8000-000000000801");
    let run_id = RunId::new_unchecked("019e7000-0000-7000-8000-000000000802");
    seed_run(&store, session_id.clone(), run_id.clone());
    let payload_id = store
        .append_provider_payload(NewProviderPayload {
            session_id,
            run_id: run_id.clone(),
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: br#"{"id":"chatcmpl_1","choices":[{"message":{"content":"fresh"}}]}"#
                .to_vec(),
            created_at: 3_000,
        })
        .expect("provider payload append should commit");
    let decoded = DecodedProviderPayload {
        status: DecodeStatus::Decoded,
        turns: vec![DecodedTurn {
            turn: Turn {
                id: MessageId::new_unchecked("019e7000-0000-7000-8000-000000000803"),
                run_id,
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 4_000,
            },
            parts: vec![DecodedPart {
                part: Part::Text {
                    text: "stale".to_string(),
                    synthetic: None,
                },
                provider_payload_id: payload_id.clone(),
                provider_json_pointer: "/choices/0/message/content".to_string(),
            }],
        }],
    };
    store
        .append_decoded_provider_payload(&payload_id, "openai-chat-completions-decoder@0", &decoded)
        .expect("old decoded projection should commit");

    let output = Command::new(env!("CARGO_BIN_EXE_nav-backend"))
        .arg("recover-payloads")
        .arg(db_path)
        .output()
        .expect("recover-payloads should run");

    assert!(
        output.status.success(),
        "recover-payloads failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("provider payload diffs: 1"));
    assert!(stdout.contains(payload_id.as_str()));
    assert!(
        stdout.contains("openai-chat-completions-decoder@0 -> openai-chat-completions-decoder@1")
    );
    assert!(stdout.contains("\"stale\""));
    assert!(stdout.contains("\"fresh\""));
}

fn seed_run(store: &SqliteSessionStore, session_id: SessionId, run_id: RunId) {
    store
        .create_session(
            session_id.clone(),
            CreateSession {
                title: None,
                source: "test".to_string(),
                workspace_root: None,
                system_prompt: None,
                settings_json: "{}".to_string(),
                parent_id: None,
                version: "test-version".to_string(),
                slug: None,
                created_at: 1_000,
            },
        )
        .expect("session create should commit");
    store
        .start_run(StartRun {
            id: run_id,
            session_id,
            status: RunStatus::Running,
            trigger: Some("test".to_string()),
            started_at: 2_000,
        })
        .expect("run start should commit");
}
