use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::types::{ResponseEnvelope, ResponseItem};

#[derive(Default)]
pub(crate) struct ResponseCollector {
    pub(super) completed: Option<ResponseEnvelope>,
    pub(super) output: Vec<ResponseItem>,
    pub(super) raw_output: Vec<Value>,
}

impl ResponseCollector {
    pub(crate) fn push_event(&mut self, event: &Value, source: &str) -> Result<bool> {
        match event.get("type").and_then(Value::as_str) {
            Some("error") => bail!("{source} returned error: {event}"),
            Some("response.completed") => {
                self.completed = Some(decode_completed_response(event)?);
                return Ok(true);
            }
            Some("response.output_item.done") => {
                let item = event
                    .get("item")
                    .cloned()
                    .context("response.output_item.done event had no item")?;
                self.raw_output.push(item.clone());
                self.output.push(
                    serde_json::from_value::<ResponseItem>(item)
                        .context("failed to decode output item")?,
                );
            }
            _ => {}
        }
        Ok(false)
    }

    pub(crate) fn finish(self, source: &str) -> Result<ResponseEnvelope> {
        let mut completed = self
            .completed
            .with_context(|| format!("{source} ended without response.completed"))?;
        if completed.output.as_ref().is_none_or(Vec::is_empty) {
            completed.output = Some(self.output);
        }
        if completed.raw_output.is_empty() {
            completed.raw_output = self.raw_output;
        }
        Ok(completed)
    }
}

pub(super) fn decode_completed_response(event: &Value) -> Result<ResponseEnvelope> {
    let response = event
        .get("response")
        .cloned()
        .context("response.completed event had no response")?;
    let mut envelope = serde_json::from_value::<ResponseEnvelope>(response.clone())
        .context("failed to decode completed response")?;
    envelope.raw_output = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(envelope)
}
