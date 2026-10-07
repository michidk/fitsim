//! Minimal HTTP/1.1 request parsing and response serialisation, independent of the socket type
//! (embassy-net on the ESP32, `std::net` on the host dev server).
//!
//! Deliberately small: no keep-alive (`Connection: close`), no chunked request bodies, no
//! pipelining. That is all a browser and `curl` need against a debugging appliance.

use alloc::string::String;
use alloc::vec::Vec;
use serde::Serialize;

/// Largest request head (request line + headers) we accept.
pub const MAX_HEAD_BYTES: usize = 2048;
/// Largest request body we accept (a 48-keyframe scenario is ~3 KB).
pub const MAX_BODY_BYTES: usize = 8192;
const MAX_HEADERS: usize = 24;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Options,
    Other,
}

impl Method {
    fn parse(s: &str) -> Self {
        match s {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            _ => Method::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHead<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: &'a str,
    pub content_length: usize,
    /// Bytes of `buf` taken by the head (the body starts here).
    pub head_len: usize,
    pub websocket_key: Option<&'a str>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HttpError {
    Malformed,
    HeadTooLarge,
    BodyTooLarge,
}

impl HttpError {
    pub fn status(self) -> u16 {
        match self {
            HttpError::Malformed => 400,
            HttpError::HeadTooLarge => 431,
            HttpError::BodyTooLarge => 413,
        }
    }
}

/// Parses a request head from the bytes received so far. `Ok(None)` means "need more data".
pub fn parse_head(buf: &[u8]) -> Result<Option<RequestHead<'_>>, HttpError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(buf) {
        Ok(httparse::Status::Partial) => {
            if buf.len() >= MAX_HEAD_BYTES {
                Err(HttpError::HeadTooLarge)
            } else {
                Ok(None)
            }
        }
        Ok(httparse::Status::Complete(head_len)) => {
            let target = req.path.ok_or(HttpError::Malformed)?;
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let mut content_length = 0usize;
            let mut ws_key = None;
            let mut upgrade_ws = false;
            for h in req.headers.iter() {
                if h.name.eq_ignore_ascii_case("content-length") {
                    content_length = core::str::from_utf8(h.value)
                        .ok()
                        .and_then(|v| v.trim().parse().ok())
                        .ok_or(HttpError::Malformed)?;
                } else if h.name.eq_ignore_ascii_case("sec-websocket-key") {
                    ws_key = core::str::from_utf8(h.value).ok().map(str::trim);
                } else if h.name.eq_ignore_ascii_case("upgrade") {
                    upgrade_ws = h.value.eq_ignore_ascii_case(b"websocket");
                } else if h.name.eq_ignore_ascii_case("transfer-encoding") {
                    return Err(HttpError::Malformed); // chunked bodies are not supported
                }
            }
            if content_length > MAX_BODY_BYTES {
                return Err(HttpError::BodyTooLarge);
            }
            Ok(Some(RequestHead {
                method: Method::parse(req.method.ok_or(HttpError::Malformed)?),
                path,
                query,
                content_length,
                head_len,
                websocket_key: if upgrade_ws { ws_key } else { None },
            }))
        }
        Err(_) => Err(HttpError::Malformed),
    }
}

/// Looks up `key` in a query string (`a=1&b=2`). No percent-decoding: our values are slugs/numbers.
pub fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then_some(v)
    })
}

#[derive(Debug)]
pub enum Body {
    Owned(Vec<u8>),
    /// Embedded asset, served without copying.
    Static(&'static [u8]),
}

impl Body {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Body::Owned(v) => v,
            Body::Static(s) => s,
        }
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Body,
    /// Body is already gzip-compressed (`Content-Encoding: gzip`).
    pub gzip: bool,
    pub cache_control: &'static str,
    pub content_disposition: Option<&'static str>,
    /// `Location` header for 3xx responses.
    pub redirect: Option<String>,
}

impl Response {
    pub fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body: Body::Owned(body),
            gzip: false,
            cache_control: "no-store",
            content_disposition: None,
            redirect: None,
        }
    }

    pub fn json<T: Serialize>(status: u16, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(v) => Self::new(status, "application/json", v),
            Err(_) => Self::error(500, "serialisation failed"),
        }
    }

    pub fn text(status: u16, body: String) -> Self {
        Self::new(status, "text/plain; charset=utf-8", body.into_bytes())
    }

    pub fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": message }))
    }

    pub fn no_content() -> Self {
        Self::new(204, "text/plain", Vec::new())
    }

    pub fn asset(content_type: &'static str, gz: &'static [u8]) -> Self {
        Self {
            status: 200,
            content_type,
            body: Body::Static(gz),
            gzip: true,
            cache_control: "no-cache",
            content_disposition: None,
            redirect: None,
        }
    }

    pub fn status_text(status: u16) -> &'static str {
        match status {
            101 => "Switching Protocols",
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            302 => "Found",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            413 => "Payload Too Large",
            415 => "Unsupported Media Type",
            422 => "Unprocessable Entity",
            431 => "Request Header Fields Too Large",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "Unknown",
        }
    }

    /// Serialises the status line and headers (CORS is always enabled so test scripts and other
    /// web apps can call the API).
    pub fn head_bytes(&self, include_body_len: bool) -> Vec<u8> {
        use core::fmt::Write;
        let mut h = String::with_capacity(256);
        let _ = write!(
            h,
            "HTTP/1.1 {} {}\r\n",
            self.status,
            Self::status_text(self.status)
        );
        let _ = write!(h, "Content-Type: {}\r\n", self.content_type);
        if include_body_len {
            let _ = write!(h, "Content-Length: {}\r\n", self.body.as_slice().len());
        }
        if self.gzip {
            h.push_str("Content-Encoding: gzip\r\n");
        }
        let _ = write!(h, "Cache-Control: {}\r\n", self.cache_control);
        if let Some(cd) = self.content_disposition {
            let _ = write!(h, "Content-Disposition: {cd}\r\n");
        }
        if let Some(loc) = &self.redirect {
            let _ = write!(h, "Location: {loc}\r\n");
        }
        h.push_str(
            "Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\n\
             Access-Control-Allow-Headers: Content-Type\r\n\
             Connection: close\r\n\r\n",
        );
        h.into_bytes()
    }
}

/// Response to a CORS preflight request.
pub fn preflight() -> Response {
    Response::no_content()
}

pub fn websocket_handshake_response(accept_key: &str) -> Vec<u8> {
    alloc::format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept_key}\r\n\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_get() {
        let raw = b"GET /api/state?x=1 HTTP/1.1\r\nHost: fitness-simulator.local\r\nUser-Agent: curl\r\n\r\n";
        let h = parse_head(raw).unwrap().unwrap();
        assert_eq!(
            (h.method, h.path, h.query, h.content_length),
            (Method::Get, "/api/state", "x=1", 0)
        );
        assert_eq!(h.head_len, raw.len());
        assert!(h.websocket_key.is_none());
    }

    #[test]
    fn parses_a_post_with_body_offset() {
        let raw = b"POST /api/state/power HTTP/1.1\r\nContent-Type: application/json\r\ncontent-length: 13\r\n\r\n{\"value\":250}";
        let h = parse_head(raw).unwrap().unwrap();
        assert_eq!(h.method, Method::Post);
        assert_eq!(h.content_length, 13);
        assert_eq!(&raw[h.head_len..], b"{\"value\":250}");
    }

    #[test]
    fn partial_heads_ask_for_more() {
        assert_eq!(parse_head(b"GET /api/st"), Ok(None));
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nHost: x\r\n"), Ok(None));
    }

    #[test]
    fn detects_websocket_upgrade() {
        let raw = b"GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
        let h = parse_head(raw).unwrap().unwrap();
        assert_eq!(h.websocket_key, Some("dGhlIHNhbXBsZSBub25jZQ=="));
        // A key without an Upgrade header is not a websocket request.
        let raw = b"GET /ws HTTP/1.1\r\nSec-WebSocket-Key: abc\r\n\r\n";
        assert!(parse_head(raw).unwrap().unwrap().websocket_key.is_none());
    }

    #[test]
    fn rejects_garbage_and_oversize() {
        assert_eq!(
            parse_head(b"\x01\x02 not http\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n"),
            Err(HttpError::BodyTooLarge)
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nContent-Length: abc\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        assert_eq!(
            parse_head(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        let mut big = b"GET / HTTP/1.1\r\nX: ".to_vec();
        big.resize(MAX_HEAD_BYTES + 1, b'a');
        assert_eq!(parse_head(&big), Err(HttpError::HeadTooLarge));
    }

    #[test]
    fn query_lookup() {
        assert_eq!(query_param("since=12&kind=ftms", "kind"), Some("ftms"));
        assert_eq!(query_param("since=12", "kind"), None);
        assert_eq!(query_param("flag", "flag"), Some(""));
    }

    #[test]
    fn response_head() {
        let r = Response::json(200, &serde_json::json!({"ok": true}));
        let head = String::from_utf8(r.head_bytes(true)).unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(head.contains("Content-Length: 11\r\n"));
        assert!(head.contains("Access-Control-Allow-Origin: *"));
        assert!(head.ends_with("\r\n\r\n"));
        let a = Response::asset("text/html", b"\x1f\x8b");
        let head = String::from_utf8(a.head_bytes(true)).unwrap();
        assert!(head.contains("Content-Encoding: gzip"));
        let mut r = Response::new(302, "text/plain", Vec::new());
        r.redirect = Some("http://192.168.4.1/".into());
        let head = String::from_utf8(r.head_bytes(true)).unwrap();
        assert!(
            head.starts_with("HTTP/1.1 302 Found\r\n")
                && head.contains("Location: http://192.168.4.1/\r\n")
        );
    }

    #[test]
    fn error_body_is_json() {
        let r = Response::error(404, "nope");
        assert_eq!(r.status, 404);
        assert_eq!(r.body.as_slice(), br#"{"error":"nope"}"#);
    }
}
