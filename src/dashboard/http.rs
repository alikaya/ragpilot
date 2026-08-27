//! A minimal HTTP/1.1 server, just enough for a localhost dashboard.
//!
//! Deliberately hand-rolled rather than pulling in a web framework: this serves
//! one page and a handful of JSON endpoints to a single user on the loopback
//! interface, and the crate's promise is to stay lean. Tokio is already here;
//! twenty more crates are not worth a page.
//!
//! No TLS, no keep-alive, no chunked encoding, no multipart. If this ever needs
//! any of that, it needs a real framework instead.

use std::collections::HashMap;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Refuse a request line or header block larger than this. A dashboard never
/// needs more, and an unbounded read on a socket is a way to be killed.
const MAX_HEAD: usize = 16 * 1024;
/// Bodies here are tiny JSON payloads.
const MAX_BODY: usize = 256 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub cookies: HashMap<String, String>,
    pub body: String,
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Extra headers, already formatted as `Name: value`.
    pub headers: Vec<String>,
}

impl Response {
    pub fn html(body: String) -> Self {
        Self { status: 200, content_type: "text/html; charset=utf-8", body: body.into_bytes(), headers: Vec::new() }
    }
    pub fn json(body: String) -> Self {
        Self { status: 200, content_type: "application/json", body: body.into_bytes(), headers: Vec::new() }
    }
    pub fn text(status: u16, body: &str) -> Self {
        Self { status, content_type: "text/plain; charset=utf-8", body: body.as_bytes().to_vec(), headers: Vec::new() }
    }
    pub fn with_header(mut self, header: String) -> Self {
        self.headers.push(header);
        self
    }
}

/// Read one request. Returns `None` on a connection that closed or sent
/// something that is not a request we can serve.
pub async fn read_request(socket: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];

    // Head first: everything up to the blank line.
    let head_end = loop {
        if let Some(i) = find_double_crlf(&buf) {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut parts = lines.next()?.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut cookies = HashMap::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.trim().parse().unwrap_or(0).min(MAX_BODY),
            "cookie" => {
                for pair in value.split(';') {
                    if let Some((k, v)) = pair.split_once('=') {
                        cookies.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }
            _ => {}
        }
    }

    // Then the body, if the head did not already carry it.
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    let (path, query) = split_target(&target);
    Some(Request {
        method,
        path,
        query,
        cookies,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

pub async fn write_response(socket: &mut TcpStream, response: Response) -> Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        response.status,
        response.content_type,
        response.body.len(),
    );
    for header in &response.headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");

    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&response.body).await?;
    socket.flush().await?;
    Ok(())
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let mut query = HashMap::new();
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    (percent_decode(path), query)
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Escape text for HTML. Project paths and note titles come from the
/// filesystem, so they are not trusted markup.
pub fn escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_splits_into_path_and_decoded_query() {
        let (path, query) = split_target("/api/state?q=hello%20world&t=abc");
        assert_eq!(path, "/api/state");
        assert_eq!(query.get("q").unwrap(), "hello world");
        assert_eq!(query.get("t").unwrap(), "abc");

        let (path, query) = split_target("/");
        assert_eq!(path, "/");
        assert!(query.is_empty());

        // A `+` is a space, and a stray `%` is left alone rather than panicking.
        assert_eq!(percent_decode("a+b%zz%2Fc"), "a b%zz/c");
        assert_eq!(percent_decode("t%C3%BCrk%C3%A7e"), "türkçe");
    }

    #[test]
    fn the_head_ends_at_the_blank_line() {
        assert_eq!(find_double_crlf(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
        assert_eq!(find_double_crlf(b"GET / HTTP/1.1\r\n"), None);
    }

    #[test]
    fn escape_neutralises_markup_from_the_filesystem() {
        assert_eq!(escape("<script>alert(1)</script>"), "&lt;script&gt;alert(1)&lt;/script&gt;");
        assert_eq!(escape(r#"a "b" & c"#), "a &quot;b&quot; &amp; c");
        assert_eq!(escape("plain"), "plain");
    }
}
