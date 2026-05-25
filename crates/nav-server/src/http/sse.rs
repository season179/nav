use nav_protocol::BackendEvent;

pub fn event_name(event: &BackendEvent) -> &'static str {
    event.event_type()
}
