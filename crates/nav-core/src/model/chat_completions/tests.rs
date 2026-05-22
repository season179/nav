//! Chat Completions transport tests.
//!
//! Wire-format coverage (request body, response parsing, SSE normalization)
//! lands with C1/C2/F2. F1 only exercises the constructor signature so the
//! module shape is locked in before the real behavior arrives.

use super::*;
use crate::context::{ModelConfig, ProviderConfig, Settings};
use crate::model::auth::resolve_provider;
use std::collections::BTreeMap;
use std::time::Duration;

fn settings_with_one_catalog_entry() -> Settings {
    let mut models = BTreeMap::new();
    models.insert("glm-5.1".to_string(), ModelConfig::default());
    let provider = ProviderConfig {
        name: Some("Z.AI".to_string()),
        base_url: Some("https://api.z.ai/v1".to_string()),
        api_key: Some("sk-zai-literal".to_string()),
        headers: None,
        models,
    };
    let mut catalog = BTreeMap::new();
    catalog.insert("z.ai".to_string(), provider);
    Settings {
        providers: Some(catalog),
        ..Settings::default()
    }
}

#[test]
fn constructor_accepts_resolved_provider() {
    let settings = settings_with_one_catalog_entry();
    let resolved = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap();
    let client = reqwest::Client::new();
    let transport = ChatCompletionsTransport::new(
        client,
        resolved,
        Duration::from_secs(60),
        RetryPolicy::default(),
    );
    // The transport is held as `dyn ResponsesTransport` by the agent loop;
    // this coercion confirms the trait is implemented and the constructor
    // signature matches what `OpenAiTransport` offers.
    let _erased: &dyn ResponsesTransport = &transport;
}
