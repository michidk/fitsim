//! HTTP/1.1 + WebSocket server on embassy-net TCP sockets.
//!
//! A fixed pool of worker tasks each owns one listening socket. Parsing, routing and the event
//! stream are the shared, host-tested code in `fitsim-core` / `fitsim-web`; this file only moves
//! bytes. Connections are `Connection: close`, except WebSockets.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use embassy_futures::select::{Either, select};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, with_timeout};
use embedded_io_async::Write;
use fitsim_core::api::Request;
use fitsim_core::http::{self, Method, Response};
use fitsim_core::ws;
use fitsim_web::RouteContext;
use portable_atomic::{AtomicU8, Ordering};

use crate::shared::{SCAN_LOCK, SCAN_REQUEST, SCAN_RESULT, Shared};
use crate::system::apply_effects;

/// Concurrent connections. One is taken by each open dashboard (WebSocket). The classic ESP32
/// has by far the least static RAM, so it gets fewer workers and smaller socket buffers.
#[cfg(feature = "esp32")]
pub const WORKERS: usize = 3;
#[cfg(not(feature = "esp32"))]
pub const WORKERS: usize = 5;
/// WebSockets are limited so the REST API always keeps a few workers.
#[cfg(feature = "esp32")]
const MAX_WEBSOCKETS: u8 = 1;
#[cfg(not(feature = "esp32"))]
const MAX_WEBSOCKETS: u8 = 2;
#[cfg(feature = "esp32")]
const RX_BUF: usize = 1024;
#[cfg(not(feature = "esp32"))]
const RX_BUF: usize = 1536;
#[cfg(feature = "esp32")]
const TX_BUF: usize = 1536;
#[cfg(not(feature = "esp32"))]
const TX_BUF: usize = 2048;

static WS_OPEN: AtomicU8 = AtomicU8::new(0);

#[embassy_executor::task(pool_size = WORKERS)]
pub async fn worker(stack: Stack<'static>, shared: Shared) {
    // Socket buffers live on the heap (leaked: they last as long as the worker) so they do not
    // eat into scarce static RAM.
    let rx: &'static mut [u8] = alloc::vec![0u8; RX_BUF].leak();
    let tx: &'static mut [u8] = alloc::vec![0u8; TX_BUF].leak();
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(Duration::from_secs(10)));
    loop {
        if socket.accept(80).await.is_err() {
            socket.abort();
            continue;
        }
        serve(&mut socket, shared).await;
        let _ = socket.flush().await;
        socket.close();
        embassy_time::Timer::after(Duration::from_millis(20)).await;
        socket.abort();
    }
}

struct Head {
    method: Method,
    path: String,
    query: String,
    ws_key: Option<String>,
    head_len: usize,
    content_length: usize,
}

/// Reads until a complete request head is buffered. Returns the parsed head and bytes buffered.
async fn read_head(socket: &mut TcpSocket<'_>, buf: &mut [u8]) -> Result<(Head, usize), Response> {
    let mut filled = 0;
    loop {
        match http::parse_head(&buf[..filled]) {
            Ok(Some(h)) => {
                return Ok((
                    Head {
                        method: h.method,
                        path: h.path.to_string(),
                        query: h.query.to_string(),
                        ws_key: h.websocket_key.map(str::to_string),
                        head_len: h.head_len,
                        content_length: h.content_length,
                    },
                    filled,
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(Response::error(e.status(), "bad request")),
        }
        if filled == buf.len() {
            return Err(Response::error(431, "request head too large"));
        }
        match socket.read(&mut buf[filled..]).await {
            Ok(0) | Err(_) => return Err(Response::error(400, "connection closed")),
            Ok(n) => filled += n,
        }
    }
}

async fn send(socket: &mut TcpSocket<'_>, head_only: bool, r: &Response) {
    if socket.write_all(&r.head_bytes(true)).await.is_err() || head_only {
        return;
    }
    let _ = socket.write_all(r.body.as_slice()).await;
}

async fn serve(socket: &mut TcpSocket<'_>, shared: Shared) {
    let mut buf = alloc::vec![0u8; http::MAX_HEAD_BYTES];
    let (head, filled) = match read_head(socket, &mut buf).await {
        Ok(v) => v,
        Err(r) => return send(socket, false, &r).await,
    };

    if head.path == "/ws" {
        return match head.ws_key {
            Some(key) => websocket(socket, shared, &key).await,
            None => {
                send(
                    socket,
                    false,
                    &Response::error(400, "websocket upgrade required"),
                )
                .await
            }
        };
    }

    // Body: whatever followed the head in the first read, plus the rest.
    let mut body: Vec<u8> = Vec::with_capacity(head.content_length);
    let have = filled
        .saturating_sub(head.head_len)
        .min(head.content_length);
    body.extend_from_slice(&buf[head.head_len..head.head_len + have]);
    let mut chunk = alloc::vec![0u8; 512];
    while body.len() < head.content_length {
        let want = (head.content_length - body.len()).min(chunk.len());
        match socket.read(&mut chunk[..want]).await {
            Ok(0) | Err(_) => {
                return send(socket, false, &Response::error(400, "truncated body")).await;
            }
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }

    if head.method == Method::Get && head.path == "/api/wifi/scan" {
        return send(socket, false, &wifi_scan(shared).await).await;
    }

    let req = Request {
        method: head.method,
        path: &head.path,
        query: &head.query,
        body: &body,
    };
    let out = shared.with(|sim| {
        fitsim_web::route(
            sim,
            &req,
            RouteContext {
                captive_portal: shared.portal,
            },
        )
    });
    send(socket, head.method == Method::Head, &out.response).await;
    if !out.effects.is_empty() {
        let _ = socket.flush().await;
        apply_effects(shared, out.effects).await;
    }
}

/// `GET /api/wifi/scan`: asks the Wi-Fi task (which owns the radio controller) to scan.
async fn wifi_scan(shared: Shared) -> Response {
    let _guard = SCAN_LOCK.lock().await;
    SCAN_RESULT.reset();
    SCAN_REQUEST.signal(());
    match with_timeout(Duration::from_secs(15), SCAN_RESULT.wait()).await {
        Ok(Ok(nets)) => {
            let list: Vec<serde_json::Value> = nets
                .iter()
                .map(|n| serde_json::json!({ "ssid": n.ssid, "rssi": n.rssi, "secure": n.secure }))
                .collect();
            Response::json(
                200,
                &serde_json::json!({ "networks": list, "provisioning": shared.portal }),
            )
        }
        Ok(Err(msg)) => Response::error(503, msg),
        Err(_) => Response::error(503, "scan timed out"),
    }
}

async fn websocket(socket: &mut TcpSocket<'_>, shared: Shared, key: &str) {
    if WS_OPEN.fetch_add(1, Ordering::AcqRel) >= MAX_WEBSOCKETS {
        WS_OPEN.fetch_sub(1, Ordering::AcqRel);
        return send(
            socket,
            false,
            &Response::error(503, "too many dashboards open"),
        )
        .await;
    }
    ws_session(socket, shared, key).await;
    WS_OPEN.fetch_sub(1, Ordering::AcqRel);
}

async fn ws_session(socket: &mut TcpSocket<'_>, shared: Shared, key: &str) {
    if socket
        .write_all(&http::websocket_handshake_response(&ws::accept_key(key)))
        .await
        .is_err()
    {
        return;
    }
    let mut cursor = ws::Cursor::default();
    let mut inbox: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 128];
    loop {
        let frames = shared.with(|sim| ws::poll_frames(sim, &mut cursor, true));
        for f in frames {
            if socket.write_all(&ws::text_frame(&f)).await.is_err() {
                return;
            }
        }
        // Wait for client data (ping/close) or the next poll.
        match select(
            socket.read(&mut chunk),
            embassy_time::Timer::after(Duration::from_millis(ws::POLL_MS)),
        )
        .await
        {
            Either::First(Ok(0) | Err(_)) => return,
            Either::First(Ok(n)) => inbox.extend_from_slice(&chunk[..n]),
            Either::Second(()) => {}
        }
        loop {
            match ws::decode_frame(&inbox) {
                Ok(Some((frame, used))) => {
                    inbox.drain(..used);
                    match frame.opcode {
                        ws::opcode::PING => {
                            let mut out = Vec::new();
                            ws::encode_frame(ws::opcode::PONG, &frame.payload, &mut out);
                            if socket.write_all(&out).await.is_err() {
                                return;
                            }
                        }
                        ws::opcode::CLOSE => {
                            let mut out = Vec::new();
                            ws::encode_frame(ws::opcode::CLOSE, &[], &mut out);
                            let _ = socket.write_all(&out).await;
                            return;
                        }
                        _ => {}
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
}
