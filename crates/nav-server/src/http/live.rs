use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{HttpRequest, HttpResponse, HttpServer};

const MAX_HTTP_BODY_BYTES: usize = 10 * 1024 * 1024;

pub fn serve(mut server: HttpServer) -> Result<()> {
    let listener = TcpListener::bind(&server.config().bind_addr)
        .with_context(|| format!("bind HTTP server to {}", server.config().bind_addr))?;
    let base_url = format!("http://{}", listener.local_addr()?);
    write_bootstrap(&base_url)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = handle_connection(&mut server, stream) {
                    eprintln!("nav-backend HTTP connection failed: {error:#}");
                }
            }
            Err(error) => eprintln!("nav-backend HTTP accept failed: {error}"),
        }
    }

    Ok(())
}

fn write_bootstrap(base_url: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &json!({
            "type": "backend.ready",
            "baseUrl": base_url,
        }),
    )?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn handle_connection(server: &mut HttpServer, mut stream: TcpStream) -> Result<()> {
    let request = {
        let mut reader = BufReader::new(&mut stream);
        match read_http_request(&mut reader)? {
            Some(request) => request,
            None => return Ok(()),
        }
    };

    let response = server.handle_request(request);
    write_http_response(&mut stream, response)
}

fn read_http_request(reader: &mut impl BufRead) -> Result<Option<HttpRequest>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }

    let request_line = trim_http_line(&request_line);
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .context("HTTP request missing method")?
        .to_string();
    let target = parts.next().context("HTTP request missing path")?;
    let path = target.split_once('?').map_or(target, |(path, _)| path);

    let mut content_length = 0usize;
    let mut last_event_id = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = trim_http_line(&line);
        if line.is_empty() {
            break;
        }

        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                content_length = value.trim().parse().context("invalid Content-Length")?;
            }
            "last-event-id" => {
                last_event_id = Some(value.trim().to_string());
            }
            _ => {}
        }
    }

    if content_length > MAX_HTTP_BODY_BYTES {
        bail!("HTTP Content-Length {content_length} exceeds limit of {MAX_HTTP_BODY_BYTES} bytes");
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let body = String::from_utf8(body).context("HTTP body was not UTF-8")?;

    Ok(Some(HttpRequest {
        method,
        path: path.to_string(),
        body,
        last_event_id,
    }))
}

fn write_http_response(writer: &mut impl Write, response: HttpResponse) -> Result<()> {
    let body = response.body();
    write!(
        writer,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status(),
        status_reason(response.status()),
        response.content_type(),
        body.len(),
        body
    )?;
    writer.flush()?;
    Ok(())
}

fn trim_http_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;

    #[test]
    fn parses_http_request_body_and_last_event_id() {
        let raw = concat!(
            "POST /rpc HTTP/1.1\r\n",
            "Host: 127.0.0.1\r\n",
            "Content-Length: 17\r\n",
            "Last-Event-ID: 019f2f6f-f17b-7a72-9f28-7f9aa0a1c853\r\n",
            "\r\n",
            r#"{"jsonrpc":"2.0"}"#
        );
        let mut reader = BufReader::new(raw.as_bytes());

        let request = read_http_request(&mut reader)
            .expect("request should parse")
            .expect("request should be present");

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/rpc");
        assert_eq!(request.body, r#"{"jsonrpc":"2.0"}"#);
        assert_eq!(
            request.last_event_id.as_deref(),
            Some("019f2f6f-f17b-7a72-9f28-7f9aa0a1c853")
        );
    }

    #[test]
    fn rejects_http_request_body_over_limit() {
        let raw = format!(
            "POST /rpc HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_HTTP_BODY_BYTES + 1
        );
        let mut reader = BufReader::new(raw.as_bytes());

        let error = read_http_request(&mut reader).expect_err("oversized body should be rejected");

        assert!(error.to_string().contains("exceeds limit"));
    }
}
