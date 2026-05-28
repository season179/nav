use nav_types::{
    ArtifactId, ArtifactRow, MessageId, PartId, ProviderPayloadId, ProviderPayloadRow, RunId,
    RunRow, SessionId, SessionRow, TurnPartRow, TurnRow,
};

#[test]
fn storage_row_skeletons_are_public_and_constructible() {
    let session_id = SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
    let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap();
    let turn_id = MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000003").unwrap();
    let part_id = PartId::try_new("prt_0000018bcfe56800_0000000000000001").unwrap();
    let artifact_id = ArtifactId::try_new("art_0000018bcfe56800_0000000000000002").unwrap();
    let provider_payload_id =
        ProviderPayloadId::try_new("pay_0000018bcfe56800_0000000000000003").unwrap();

    let session = SessionRow {
        id: session_id.clone(),
        title: Some("Storage smoke".to_string()),
        source: "tui".to_string(),
        workspace_root: Some("/tmp/nav".to_string()),
        system_prompt: None,
        settings_json: "{}".to_string(),
        parent_id: None,
        version: "26.5.12".to_string(),
        slug: Some("storage-smoke".to_string()),
        cost: 0.0,
        tokens_input: 0,
        tokens_output: 0,
        tokens_reasoning: 0,
        tokens_cache_read: 0,
        tokens_cache_write: 0,
        time_archived: None,
        time_compacting: None,
        revert_json: None,
        created_at: 100,
        updated_at: 101,
    };
    let run = RunRow {
        id: run_id.clone(),
        session_id: session_id.clone(),
        status: "completed".to_string(),
        trigger: Some("user".to_string()),
        started_at: 102,
        finished_at: Some(103),
        error_json: None,
    };
    let turn = TurnRow {
        id: turn_id.clone(),
        run_id: run_id.clone(),
        seq: 0,
        role: "assistant".to_string(),
        meta_json: "{}".to_string(),
        created_at: 104,
    };
    let part = TurnPartRow {
        id: part_id.clone(),
        turn_id: turn_id.clone(),
        session_id: session_id.clone(),
        part_type: "text".to_string(),
        data_json: r#"{"text":"hello"}"#.to_string(),
        provider_payload_id: Some(provider_payload_id.clone()),
        provider_json_pointer: Some("/choices/0/message".to_string()),
        compacted_at: None,
        created_at: 105,
    };
    let artifact = ArtifactRow {
        id: artifact_id.clone(),
        session_id: session_id.clone(),
        part_id: Some(part_id),
        kind: "tool_output".to_string(),
        mime: "text/plain".to_string(),
        sha256: "abc123".to_string(),
        path: "blobs/ab/abc123".to_string(),
        size_bytes: 12,
        created_at: 106,
    };
    let provider_payload = ProviderPayloadRow {
        id: provider_payload_id,
        session_id,
        run_id,
        direction: "response".to_string(),
        api_kind: "openai_chat_completions".to_string(),
        provider_id: Some("openai".to_string()),
        model_id: Some("gpt-5.1".to_string()),
        sequence: 0,
        provider_payload_id: Some("chatcmpl_123".to_string()),
        artifact_id,
        sha256: "def456".to_string(),
        decoder_version: Some("v1".to_string()),
        decode_status: "decoded".to_string(),
        error_json: None,
        created_at: 107,
        decoded_at: Some(108),
    };

    assert_eq!(session.id.as_str(), "019f2f6f-f178-7a72-9f28-000000000001");
    assert_eq!(run.session_id, session.id);
    assert_eq!(turn.run_id, run.id);
    assert_eq!(part.turn_id, turn.id);
    assert_eq!(artifact.session_id, run.session_id);
    assert_eq!(provider_payload.artifact_id, artifact.id);
}
