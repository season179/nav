use nav_protocol::{BackendEvent, EventEnvelope};

pub fn event_name(event: &BackendEvent) -> &'static str {
    event.event_type()
}

pub fn encode_events(events: &[EventEnvelope]) -> Result<String, serde_json::Error> {
    let mut body = String::new();

    for envelope in events {
        body.push_str("id: ");
        body.push_str(envelope.event_id.as_str());
        body.push('\n');
        body.push_str("event: ");
        body.push_str(envelope.event_type());
        body.push('\n');
        body.push_str("data: ");
        body.push_str(&serde_json::to_string(envelope)?);
        body.push_str("\n\n");
    }

    Ok(body)
}
