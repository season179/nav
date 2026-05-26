#![allow(dead_code)]

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Debug)]
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
        let request_for_thread = Arc::clone(&request);
        let handle = thread::spawn(move || {
            if let Some(stream) = accept_provider_request(listener) {
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
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn request(mut self) -> ProviderRequest {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("fake provider thread should finish");
        }
        self.request
            .lock()
            .unwrap()
            .take()
            .expect("fake provider should receive a request")
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
            if let Some(stream) = accept_provider_request(listener) {
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(request) = self.request.lock().unwrap().take() {
                return request;
            }
            assert!(
                Instant::now() < deadline,
                "hanging provider did not receive a request"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .expect("hanging provider thread should finish");
        }
    }
}

pub fn provider_sse_chunk(json: &str) -> String {
    format!("data: {json}\n\n")
}

pub fn successful_provider_with_text(text: &str) -> FakeProviderServer {
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

    FakeProviderServer::start(
        200,
        "text/event-stream",
        vec![
            provider_sse_chunk(&text_chunk.to_string()),
            provider_sse_chunk(&completed_chunk.to_string()),
            "data: [DONE]\n\n".to_string(),
        ],
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

fn accept_provider_request(listener: TcpListener) -> Option<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
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
