#![allow(dead_code)]

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nav_server::http::{HttpServer, RunStatus};
use nav_types::RunId;
use serde_json::{Value, json};

const DELAYED_CHAT_COMPLETIONS_STREAM: &str =
    include_str!("../../../../fixtures/protocol/provider-streams/delayed-chat-completions.sse");
const DELAYED_BOUNDARY_MARKER: &str = ": delayed boundary";

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub path: String,
    pub body: Value,
    headers: Vec<(String, String)>,
}

impl ProviderRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
pub struct FakeProviderServer {
    addr: String,
    request: Arc<Mutex<Option<ProviderRequest>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub struct SequencedProviderServer {
    addr: String,
    requests: Arc<Mutex<Vec<ProviderRequest>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub struct DelayedProviderServer {
    addr: String,
    request: Arc<Mutex<Option<ProviderRequest>>>,
    stop: Arc<AtomicBool>,
    release: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl FakeProviderServer {
    pub fn start(status: u16, content_type: &'static str, chunks: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake provider should bind");
        listener
            .set_nonblocking(true)
            .expect("fake provider should support nonblocking accept");
        let addr = listener.local_addr().unwrap().to_string();
        let request = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let request_for_thread = Arc::clone(&request);
        let stop_for_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            if let Some(stream) = accept_provider_request(&listener, &stop_for_thread) {
                handle_provider_connection(
                    stream,
                    request_for_thread,
                    status,
                    content_type,
                    chunks,
                );
            }
        });

        Self {
            addr,
            request,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn request(mut self) -> ProviderRequest {
        self.join_thread()
            .expect("fake provider thread should finish");
        self.request
            .lock()
            .unwrap()
            .take()
            .expect("fake provider should receive a request")
    }

    fn join_thread(&mut self) -> thread::Result<()> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for FakeProviderServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.join_thread();
    }
}

impl SequencedProviderServer {
    pub fn start(responses: Vec<Vec<String>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake provider should bind");
        listener
            .set_nonblocking(true)
            .expect("fake provider should support nonblocking accept");
        let addr = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_for_thread = Arc::clone(&requests);
        let stop_for_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            for chunks in responses {
                if let Some(stream) = accept_provider_request(&listener, &stop_for_thread) {
                    handle_queued_provider_connection(
                        stream,
                        Arc::clone(&requests_for_thread),
                        200,
                        "text/event-stream",
                        chunks,
                    );
                }
            }
        });

        Self {
            addr,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn requests(mut self) -> Vec<ProviderRequest> {
        self.join_thread()
            .expect("fake provider thread should finish");
        std::mem::take(&mut *self.requests.lock().unwrap())
    }

    fn join_thread(&mut self) -> thread::Result<()> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for SequencedProviderServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.join_thread();
    }
}

impl DelayedProviderServer {
    fn start(first_chunks: Vec<String>, delayed_chunks: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("delayed provider should bind");
        listener
            .set_nonblocking(true)
            .expect("delayed provider should support nonblocking accept");
        let addr = listener.local_addr().unwrap().to_string();
        let request = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = mpsc::channel();
        let request_for_thread = Arc::clone(&request);
        let stop_for_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            if let Some(stream) = accept_provider_request(&listener, &stop_for_thread) {
                handle_delayed_provider_connection(
                    stream,
                    request_for_thread,
                    stop_for_thread,
                    release_rx,
                    first_chunks,
                    delayed_chunks,
                );
            }
        });

        Self {
            addr,
            request,
            stop,
            release: Some(release_tx),
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn wait_for_request(&self) -> ProviderRequest {
        wait_for_provider_request(&self.request, "delayed provider")
    }

    pub fn release_completion(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release_completion();
        let _ = self.join_thread();
    }

    fn join_thread(&mut self) -> thread::Result<()> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for DelayedProviderServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
pub struct HangingProviderServer {
    addr: String,
    request: Arc<Mutex<Option<ProviderRequest>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HangingProviderServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("hanging provider should bind");
        listener
            .set_nonblocking(true)
            .expect("hanging provider should support nonblocking accept");
        let addr = listener.local_addr().unwrap().to_string();
        let request = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let request_for_thread = Arc::clone(&request);
        let stop_for_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            if let Some(stream) = accept_provider_request(&listener, &stop_for_thread) {
                handle_hanging_provider_connection(stream, request_for_thread, stop_for_thread);
            }
        });

        Self {
            addr,
            request,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn wait_for_request(&self) -> ProviderRequest {
        wait_for_provider_request(&self.request, "hanging provider")
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.join_thread()
            .expect("hanging provider thread should finish");
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.join_thread();
    }

    fn join_thread(&mut self) -> thread::Result<()> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for HangingProviderServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn wait_for_run_status(server: &HttpServer, run_id: &str, expected: RunStatus) {
    let run_id = RunId::try_new(run_id).expect("run id should be valid");
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        if server.run_status(&run_id) == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} did not reach {}",
            expected.as_str()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn provider_sse_chunk(json: &str) -> String {
    format!("data: {json}\n\n")
}

pub fn successful_provider_with_text(text: &str) -> FakeProviderServer {
    FakeProviderServer::start(200, "text/event-stream", successful_provider_chunks(text))
}

pub fn successful_provider_chunks(text: &str) -> Vec<String> {
    let text_chunk = json!({
        "id": "provider-run",
        "model": "vendor/model-large",
        "choices": [{
            "index": 0,
            "delta": { "content": text },
            "finish_reason": null
        }]
    });
    let completed_chunk = json!({
        "id": "provider-run",
        "model": "vendor/model-large",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });

    vec![
        provider_sse_chunk(&text_chunk.to_string()),
        provider_sse_chunk(&completed_chunk.to_string()),
        "data: [DONE]\n\n".to_string(),
    ]
}

pub fn delayed_chat_completions_provider() -> DelayedProviderServer {
    let (first, delayed) = DELAYED_CHAT_COMPLETIONS_STREAM
        .split_once(DELAYED_BOUNDARY_MARKER)
        .expect("delayed provider fixture should include a boundary marker");

    DelayedProviderServer::start(
        provider_stream_frames(first),
        provider_stream_frames(delayed),
    )
}

pub fn unused_local_base_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test should reserve a local port");
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}/v1")
}

fn handle_provider_connection(
    stream: TcpStream,
    request: Arc<Mutex<Option<ProviderRequest>>>,
    status: u16,
    content_type: &str,
    chunks: Vec<String>,
) {
    let (provider_request, mut stream) = read_provider_request(stream);
    *request.lock().unwrap() = Some(provider_request);

    let body = chunks.concat();
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("fake provider response should write");
    stream.flush().expect("fake provider response should flush");
}

fn handle_queued_provider_connection(
    stream: TcpStream,
    requests: Arc<Mutex<Vec<ProviderRequest>>>,
    status: u16,
    content_type: &str,
    chunks: Vec<String>,
) {
    let (provider_request, mut stream) = read_provider_request(stream);
    requests.lock().unwrap().push(provider_request);

    let body = chunks.concat();
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("fake provider response should write");
    stream.flush().expect("fake provider response should flush");
}

fn handle_delayed_provider_connection(
    stream: TcpStream,
    request: Arc<Mutex<Option<ProviderRequest>>>,
    stop: Arc<AtomicBool>,
    release: mpsc::Receiver<()>,
    first_chunks: Vec<String>,
    delayed_chunks: Vec<String>,
) {
    let (provider_request, mut stream) = read_provider_request(stream);
    *request.lock().unwrap() = Some(provider_request);

    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .expect("delayed provider headers should write");
    stream
        .flush()
        .expect("delayed provider headers should flush");

    if !write_provider_chunks(&mut stream, first_chunks, "first") {
        return;
    }

    loop {
        match release.recv_timeout(Duration::from_millis(10)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if stop.load(Ordering::SeqCst) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }

    if stop.load(Ordering::SeqCst) {
        return;
    }

    write_provider_chunks(&mut stream, delayed_chunks, "delayed");
}

fn handle_hanging_provider_connection(
    stream: TcpStream,
    request: Arc<Mutex<Option<ProviderRequest>>>,
    stop: Arc<AtomicBool>,
) {
    let (provider_request, mut stream) = read_provider_request(stream);
    *request.lock().unwrap() = Some(provider_request);

    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .expect("hanging provider headers should write");
    stream
        .flush()
        .expect("hanging provider headers should flush");

    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn provider_stream_frames(input: &str) -> Vec<String> {
    input
        .split("\n\n")
        .filter_map(|frame| {
            let lines = frame
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.is_empty() && !line.starts_with(':'))
                .collect::<Vec<_>>();

            (!lines.is_empty()).then(|| format!("{}\n\n", lines.join("\n")))
        })
        .collect()
}

fn wait_for_provider_request(
    request: &Arc<Mutex<Option<ProviderRequest>>>,
    provider_name: &str,
) -> ProviderRequest {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(request) = request.lock().unwrap().take() {
            return request;
        }
        assert!(
            Instant::now() < deadline,
            "{provider_name} did not receive a request"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn write_provider_chunks(stream: &mut TcpStream, chunks: Vec<String>, phase: &str) -> bool {
    for chunk in chunks {
        if stop_sending_on_broken_pipe(
            stream.write_all(chunk.as_bytes()),
            &format!("{phase} chunk should write"),
        ) {
            return false;
        }
        if stop_sending_on_broken_pipe(stream.flush(), &format!("{phase} chunk should flush")) {
            return false;
        }
    }

    true
}

fn stop_sending_on_broken_pipe(result: std::io::Result<()>, context: &str) -> bool {
    match result {
        Ok(()) => false,
        Err(error) if error.kind() == ErrorKind::BrokenPipe => true,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn accept_provider_request(listener: &TcpListener, stop: &AtomicBool) -> Option<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }

        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("accepted provider stream should become blocking");
                return Some(stream);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return None,
            Err(error) => panic!("fake provider accept failed: {error}"),
        }
    }
}

fn read_provider_request(stream: TcpStream) -> (ProviderRequest, TcpStream) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("provider request line should read");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .expect("provider request should include path")
        .to_string();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("provider header should read");
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().expect("content-length should parse");
            }
            headers.push((name, value));
        }
    }

    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .expect("provider body should read");
    let body = serde_json::from_slice(&body).expect("provider body should be JSON");

    (
        ProviderRequest {
            path,
            headers,
            body,
        },
        reader.into_inner(),
    )
}
