use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

pub const FIXTURE_SESSION_ID: &str = "019f2f6f-f178-7a72-9f28-000000000100";
const FIXTURE_RUN_ID: &str = "019f2f6f-f178-7a72-9f28-000000000200";
const FIXTURE_MESSAGE_ID: &str = "019f2f6f-f178-7a72-9f28-000000000300";
const SESSION_CREATED_EVENT_ID: &str = "019f2f6f-f178-7a72-9f28-000000000101";
const RUN_STARTED_EVENT_ID: &str = "019f2f6f-f178-7a72-9f28-000000000102";
const MESSAGE_DELTA_EVENT_ID: &str = "019f2f6f-f178-7a72-9f28-000000000103";
const MESSAGE_COMPLETED_EVENT_ID: &str = "019f2f6f-f178-7a72-9f28-000000000104";
const RUN_COMPLETED_EVENT_ID: &str = "019f2f6f-f178-7a72-9f28-000000000105";

pub struct BackendConfig {
    pub bind_address: String,
}

impl BackendConfig {
    pub fn from_args(args: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut bind_address = String::from("127.0.0.1:0");
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bind" => {
                    bind_address = args.next().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "--bind requires an address")
                    })?;
                }
                "--help" | "-h" => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "usage: nav-local-backend [--bind 127.0.0.1:0]",
                    ));
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown argument: {arg}"),
                    ));
                }
            }
        }

        Ok(Self { bind_address })
    }
}

pub fn serve(listener: TcpListener) -> io::Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    let _ = handle_connection(stream);
                });
            }
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

fn handle_connection(mut stream: TcpStream) -> io::Result<()> {
    let request = read_request(&mut stream)?;
    let fixture_events_path = format!("/sessions/{FIXTURE_SESSION_ID}/events");

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", path) if path == fixture_events_path => write_fixture_event_stream(&mut stream),
        ("POST", "/rpc") => write_json_response(
            &mut stream,
            "501 Not Implemented",
            r#"{"error":"rpc_deferred","message":"POST /rpc is reserved for JSON-RPC commands, but this minimal backend only exposes the deterministic read-only SSE fixture."}"#,
        ),
        _ => write_json_response(
            &mut stream,
            "404 Not Found",
            r#"{"error":"not_found","message":"unknown local backend route"}"#,
        ),
    }
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line)? == 0 {
            break;
        }
        if header_line == "\r\n" || header_line == "\n" {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    Ok(Request { method, path })
}

fn write_fixture_event_stream(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\n\
          content-type: text/event-stream\r\n\
          cache-control: no-cache\r\n\
          connection: close\r\n\
          \r\n",
    )?;

    for event in fixture_events() {
        write!(
            stream,
            "id: {}\nevent: {}\ndata: {}\n\n",
            event.id, event.name, event.data
        )?;
    }

    stream.flush()
}

fn write_json_response(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\n\
         connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )?;
    stream.flush()
}

fn fixture_events() -> Vec<FixtureEvent> {
    vec![
        FixtureEvent {
            id: SESSION_CREATED_EVENT_ID,
            name: "session.created",
            data: format!(
                r#"{{"event_id":"{SESSION_CREATED_EVENT_ID}","session_id":"{FIXTURE_SESSION_ID}","type":"session.created","sequence":0}}"#
            ),
        },
        FixtureEvent {
            id: RUN_STARTED_EVENT_ID,
            name: "run.started",
            data: format!(
                r#"{{"event_id":"{RUN_STARTED_EVENT_ID}","session_id":"{FIXTURE_SESSION_ID}","type":"run.started","sequence":1,"run_id":"{FIXTURE_RUN_ID}"}}"#
            ),
        },
        FixtureEvent {
            id: MESSAGE_DELTA_EVENT_ID,
            name: "message.delta",
            data: format!(
                r#"{{"event_id":"{MESSAGE_DELTA_EVENT_ID}","session_id":"{FIXTURE_SESSION_ID}","type":"message.delta","sequence":2,"run_id":"{FIXTURE_RUN_ID}","message_id":"{FIXTURE_MESSAGE_ID}","text":"Hello from the deterministic nav local backend fixture."}}"#
            ),
        },
        FixtureEvent {
            id: MESSAGE_COMPLETED_EVENT_ID,
            name: "message.completed",
            data: format!(
                r#"{{"event_id":"{MESSAGE_COMPLETED_EVENT_ID}","session_id":"{FIXTURE_SESSION_ID}","type":"message.completed","sequence":3,"run_id":"{FIXTURE_RUN_ID}","message_id":"{FIXTURE_MESSAGE_ID}","finish_reason":"stop"}}"#
            ),
        },
        FixtureEvent {
            id: RUN_COMPLETED_EVENT_ID,
            name: "run.completed",
            data: format!(
                r#"{{"event_id":"{RUN_COMPLETED_EVENT_ID}","session_id":"{FIXTURE_SESSION_ID}","type":"run.completed","sequence":4,"run_id":"{FIXTURE_RUN_ID}","status":"completed"}}"#
            ),
        },
    ]
}

struct Request {
    method: String,
    path: String,
}

struct FixtureEvent {
    id: &'static str,
    name: &'static str,
    data: String,
}
