use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiKind {
    #[serde(rename = "openai-completions", alias = "openai_chat_completions")]
    OpenAiCompletions,
    #[serde(rename = "chatgpt-subscription", alias = "chatgpt_subscription")]
    ChatGptSubscription,
}
