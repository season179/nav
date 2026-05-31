use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const BACKEND_BIN: &str = env!("CARGO_BIN_EXE_nav-local-backend");
const FIXTURE_SESSION_ID: &str = "019f2f6f-f178-7a72-9f28-000000000100";
const FIRST_EVENT_ID: &str = "019f2f6f-f178-7a72-9f28-000000000101";

#[test]
fn fixture_sse_endpoint_streams_ordered_events() {
    let mut backend = TestBackend::start();

    let response = backend.request(&format!(
        "GET /sessions/{FIXTURE_SESSION_ID}/events HTTP/1.1\r\n\
         Host: localhost\r\n\
         Connection: close\r\n\
         \r\n"
    ));

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "unexpected response:\n{response}"
    );
    assert!(
        response.contains("content-type: text/event-stream"),
        "unexpected response headers:\n{response}"
    );
    assert!(
        response.contains(&format!("id: {FIRST_EVENT_ID}\nevent: session.created\n")),
        "missing first SSE frame:\n{response}"
    );
    assert!(
        response.contains(&format!(
            "\"event_id\":\"{FIRST_EVENT_ID}\",\"session_id\":\"{FIXTURE_SESSION_ID}\",\"type\":\"session.created\""
        )),
        "missing documented event envelope:\n{response}"
    );
    assert!(
        response.contains("event: message.completed"),
        "fixture stream should include a completed message event:\n{response}"
    );
}

#[test]
fn rpc_endpoint_is_explicitly_deferred() {
    let mut backend = TestBackend::start();
    let body = r#"{"jsonrpc":"2.0","id":"request-1","method":"initialize"}"#;

    let response = backend.request(&format!(
        "POST /rpc HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    ));

    assert!(
        response.starts_with("HTTP/1.1 501 Not Implemented"),
        "unexpected response:\n{response}"
    );
    assert!(
        response.contains(r#""error":"rpc_deferred""#),
        "RPC response should document the deferred command channel:\n{response}"
    );
}

struct TestBackend {
    child: Child,
    base_url: String,
}

impl TestBackend {
    fn start() -> Self {
        let mut child = Command::new(BACKEND_BIN)
            .args(["--bind", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start nav-local-backend");

        let stdout = child.stdout.take().expect("capture backend stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            let mut line = String::new();
            let result = stdout.read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        });

        let startup_output = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("backend should print its local URL")
            .expect("read backend stdout");
        let base_url = startup_output
            .lines()
            .find_map(|line| line.strip_prefix("nav local backend listening on "))
            .expect("backend should print a discoverable URL")
            .to_owned();

        Self { child, base_url }
    }

    fn request(&mut self, request: &str) -> String {
        let address = self
            .base_url
            .strip_prefix("http://")
            .expect("backend URL should be HTTP");
        let mut stream = TcpStream::connect(address).expect("connect to backend");

        stream
            .write_all(request.as_bytes())
            .expect("send HTTP request");
        stream.shutdown(Shutdown::Write).expect("finish request");

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read HTTP response");
        response
    }
}

impl Drop for TestBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
