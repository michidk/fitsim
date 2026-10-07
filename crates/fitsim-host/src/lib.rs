//! Desktop build of the fitness simulator (library part; the binary only parses arguments).
//!
//! Runs the same `Simulator`, REST router and WebSocket stream as the firmware, minus the radio.
//! Use it to develop the web UI and to script against the API without hardware:
//!
//! ```text
//! cargo run -p fitsim-host -- --port 8080
//! ```
//!
//! Extra endpoints only available here (they stand in for a BLE client):
//!
//! * `POST /api/debug/ble`  `{"event":"connect"|"disconnect"|"subscribe"|"unsubscribe","client":64,"char":"indoorBikeData"}`
//! * `POST /api/debug/ftms-write`  `{"client":64,"hex":"0500c800"}` → `{"response":"800501", ...}`

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use fitsim_core::api::{Effect, Request};
use fitsim_core::http::{self, Method, Response};
use fitsim_core::settings::Settings;
use fitsim_core::sim::{Char, Simulator, TICK_MS};
use fitsim_core::ws;
use fitsim_web::RouteContext;

type Shared = Arc<Mutex<Simulator>>;

/// Server configuration.
pub struct Options {
    pub port: u16,
    pub data_dir: PathBuf,
    pub provisioning: bool,
}

fn load_settings(dir: &Path) -> Settings {
    fs::read(dir.join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn apply_effects(sim: &Shared, dir: &Path, effects: Vec<Effect>) {
    let _ = fs::create_dir_all(dir);
    for fx in effects {
        let s = sim.lock().unwrap();
        match fx {
            Effect::PersistSettings => {
                let _ = fs::write(
                    dir.join("settings.json"),
                    serde_json::to_vec_pretty(&s.settings).unwrap(),
                );
            }
            Effect::SaveWifi(cfg) => println!(
                "[host] would store Wi-Fi credentials for '{}' and reboot",
                cfg.ssid
            ),
            Effect::FactoryReset => {
                let _ = fs::remove_dir_all(dir);
                println!("[host] factory reset: removed {}", dir.display());
            }
            Effect::Reboot => println!("[host] reboot requested (ignored on the host)"),
        }
    }
}

fn respond(stream: &mut TcpStream, head_only: bool, r: &Response) {
    let mut out = r.head_bytes(true);
    if !head_only {
        out.extend_from_slice(r.body.as_slice());
    }
    let _ = stream.write_all(&out);
}

/// Reads one full request (head + body). Returns `None` on EOF / errors.
fn read_request(stream: &mut TcpStream) -> Result<(Vec<u8>, usize, usize), Response> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        // Copy the two numbers out so the borrow of `buf` ends before we extend it.
        let parsed = http::parse_head(&buf).map(|h| h.map(|h| (h.head_len, h.content_length)));
        match parsed {
            Ok(Some((head_len, content_length))) => {
                let total = head_len + content_length;
                while buf.len() < total {
                    let n = stream
                        .read(&mut chunk)
                        .map_err(|_| Response::error(400, "read error"))?;
                    if n == 0 {
                        return Err(Response::error(400, "truncated body"));
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                return Ok((buf, head_len, content_length));
            }
            Ok(None) => {}
            Err(e) => return Err(Response::error(e.status(), "bad request")),
        }
        let n = stream
            .read(&mut chunk)
            .map_err(|_| Response::error(400, "read error"))?;
        if n == 0 {
            return Err(Response::error(400, "empty request"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn char_from(name: &str) -> Option<Char> {
    Char::ALL.into_iter().find(|c| c.name() == name)
}

/// Host-only endpoints standing in for the radio and platform.
fn debug_route(
    sim: &Shared,
    method: Method,
    path: &str,
    body: &[u8],
    provisioning: bool,
) -> Option<Response> {
    match (method, path) {
        (Method::Get, "/api/wifi/scan") => Some(Response::json(
            200,
            &serde_json::json!({ "networks": [
                {"ssid": "HomeNetwork", "rssi": -48, "secure": true},
                {"ssid": "Guest", "rssi": -67, "secure": false},
                {"ssid": "Neighbour-5G", "rssi": -81, "secure": true},
            ], "provisioning": provisioning }),
        )),
        (Method::Post, "/api/debug/ble") => {
            let v: serde_json::Value = serde_json::from_slice(body).ok()?;
            let client = v["client"].as_u64().unwrap_or(64) as u16;
            let mut s = sim.lock().unwrap();
            match v["event"].as_str()? {
                "connect" => s.client_connected(
                    client,
                    format!("AA:BB:CC:00:00:{client:02X}"),
                    v["mtu"].as_u64().unwrap_or(23) as u16,
                ),
                "disconnect" => s.client_disconnected(client),
                "subscribe" | "unsubscribe" => {
                    let ch = char_from(v["char"].as_str()?)?;
                    s.client_subscription(client, ch, v["event"] == "subscribe");
                }
                "update" => s.client_update(
                    client,
                    v["mtu"].as_u64().map(|m| m as u16),
                    v["rssi"].as_i64().map(|r| r as i8),
                ),
                _ => return Some(Response::error(400, "unknown event")),
            }
            Some(Response::json(200, &serde_json::json!({ "ok": true })))
        }
        (Method::Post, "/api/debug/ftms-write") => {
            let v: serde_json::Value = serde_json::from_slice(body).ok()?;
            let data = hex_decode(v["hex"].as_str()?)?;
            let mut s = sim.lock().unwrap();
            let out = s.ftms_control_write(v["client"].as_u64().unwrap_or(64) as u16, &data);
            Some(Response::json(
                200,
                &serde_json::json!({
                    "response": hex_encode(&out.response),
                    "status": out.status.as_deref().map(hex_encode),
                    "accepted": out.accepted,
                }),
            ))
        }
        _ => None,
    }
}

fn handle_ws(stream: &mut TcpStream, sim: &Shared, key: &str) {
    let accept = ws::accept_key(key);
    if stream
        .write_all(&http::websocket_handshake_response(&accept))
        .is_err()
    {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(ws::POLL_MS)));
    let mut cursor = ws::Cursor::default();
    let mut inbox: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let frames = ws::poll_frames(&sim.lock().unwrap(), &mut cursor, true);
        for f in frames {
            if stream.write_all(&ws::text_frame(&f)).is_err() {
                return;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => inbox.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
        loop {
            match ws::decode_frame(&inbox) {
                Ok(Some((frame, used))) => {
                    inbox.drain(..used);
                    match frame.opcode {
                        ws::opcode::PING => {
                            let mut out = Vec::new();
                            ws::encode_frame(ws::opcode::PONG, &frame.payload, &mut out);
                            let _ = stream.write_all(&out);
                        }
                        ws::opcode::CLOSE => {
                            let mut out = Vec::new();
                            ws::encode_frame(ws::opcode::CLOSE, &[], &mut out);
                            let _ = stream.write_all(&out);
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

fn handle_connection(mut stream: TcpStream, sim: Shared, opts: Arc<Options>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let (buf, head_len, content_len) = match read_request(&mut stream) {
        Ok(v) => v,
        Err(r) => return respond(&mut stream, false, &r),
    };
    let head = http::parse_head(&buf)
        .ok()
        .flatten()
        .expect("parsed once already");
    let body = &buf[head_len..head_len + content_len];

    if head.path == "/ws" {
        match head.websocket_key {
            Some(key) => handle_ws(&mut stream, &sim, key),
            None => respond(
                &mut stream,
                false,
                &Response::error(400, "websocket upgrade required"),
            ),
        }
        return;
    }
    if let Some(r) = debug_route(&sim, head.method, head.path, body, opts.provisioning) {
        return respond(&mut stream, false, &r);
    }

    let req = Request {
        method: head.method,
        path: head.path,
        query: head.query,
        body,
    };
    let out = {
        let mut s = sim.lock().unwrap();
        fitsim_web::route(
            &mut s,
            &req,
            RouteContext {
                captive_portal: opts.provisioning,
            },
        )
    };
    respond(&mut stream, head.method == Method::Head, &out.response);
    apply_effects(&sim, &opts.data_dir, out.effects);
}

/// Builds the simulator the same way `main` does.
pub fn build_simulator(opts: &Options) -> Simulator {
    let mut sim = Simulator::new(load_settings(&opts.data_dir), 0xF17_5111);
    sim.platform.chip = "host".into();
    sim.platform.heap_free = 0;
    sim.platform.wifi.mode = if opts.provisioning {
        "accessPoint"
    } else {
        "station"
    };
    sim.platform.wifi.ssid = if opts.provisioning {
        "FitnessSimulator-HOST".into()
    } else {
        "host".into()
    };
    sim.platform.wifi.ip = "127.0.0.1".into();
    sim.set_advertising(true);
    sim.log(
        fitsim_core::event::Level::Info,
        fitsim_core::event::Kind::System,
        "Host simulator started",
    );
    sim
}

/// Runs the simulation clock and serves connections on `listener` forever.
pub fn serve(listener: TcpListener, opts: Options) {
    let opts = Arc::new(opts);
    let sim: Shared = Arc::new(Mutex::new(build_simulator(&opts)));
    {
        let sim = sim.clone();
        thread::spawn(move || {
            let start = Instant::now();
            loop {
                sim.lock().unwrap().tick(start.elapsed().as_millis() as u64);
                thread::sleep(Duration::from_millis(TICK_MS));
            }
        });
    }
    for stream in listener.incoming().flatten() {
        let (sim, opts) = (sim.clone(), opts.clone());
        thread::spawn(move || handle_connection(stream, sim, opts));
    }
}
