//! End-to-end tests: the real server on a real TCP port, driven with raw sockets.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use fitsim_host::{Options, serve};

struct Server {
    port: u16,
    data_dir: PathBuf,
}

impl Server {
    fn start(provisioning: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data_dir =
            std::env::temp_dir().join(format!("fitsim-e2e-{}-{nanos}", std::process::id()));
        let opts = Options {
            port,
            data_dir: data_dir.clone(),
            provisioning,
        };
        thread::spawn(move || serve(listener, opts));
        Self { port, data_dir }
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Response {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(s, "{method} {path} HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        Response::parse(&raw)
    }

    fn get(&self, path: &str) -> Response {
        self.request("GET", path, "")
    }

    fn post(&self, path: &str, body: &str) -> Response {
        self.request("POST", path, body)
    }
}

struct Response {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Response {
    fn parse(raw: &[u8]) -> Self {
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("complete response head")
            + 4;
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        Self {
            status,
            headers: head.to_ascii_lowercase(),
            body: raw[split..].to_vec(),
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
}

fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn serves_the_embedded_gzip_ui() {
    let srv = Server::start(false);
    let r = srv.get("/");
    assert_eq!(r.status, 200);
    assert!(r.headers.contains("content-encoding: gzip"));
    assert!(r.headers.contains("content-type: text/html"));
    assert_eq!(&r.body[..2], [0x1f, 0x8b], "gzip magic");
    // SPA fallback and 404 for unknown files
    assert_eq!(srv.get("/trainer").status, 200);
    assert_eq!(srv.get("/nope.png").status, 404);
}

#[test]
fn rest_changes_show_up_in_state() {
    let srv = Server::start(false);
    assert_eq!(srv.post("/api/state/power", r#"{"value":250}"#).status, 200);
    assert_eq!(
        srv.post("/api/state/heart-rate", r#"{"value":155}"#).status,
        200
    );
    // The simulation clock ticks every 50 ms, so wait for both changes to be applied.
    let state = eventually("telemetry to follow", || {
        let s = srv.get("/api/state").json();
        (s["telemetry"]["power"] == 250.0 && s["telemetry"]["heartRate"] == 155.0).then_some(s)
    });
    assert_eq!(state["settings"]["deviceName"], "DebugTrainer");

    let bad = srv.post("/api/state/power", r#"{"value":99999}"#);
    assert_eq!(bad.status, 422);
    assert!(bad.json()["error"].as_str().unwrap().contains("Power"));
    assert_eq!(srv.get("/api/state/nonsense").status, 404);
}

#[test]
fn cors_preflight_and_headers() {
    let srv = Server::start(false);
    let r = srv.request("OPTIONS", "/api/state/power", "");
    assert_eq!(r.status, 204);
    assert!(r.headers.contains("access-control-allow-origin: *"));
    assert!(r.headers.contains("access-control-allow-methods"));
}

#[test]
fn ftms_control_flow_is_visible_through_the_api() {
    let srv = Server::start(false);
    srv.post(
        "/api/debug/ble",
        r#"{"event":"connect","client":64,"mtu":247}"#,
    );
    srv.post(
        "/api/debug/ble",
        r#"{"event":"subscribe","client":64,"char":"controlPoint"}"#,
    );
    let req = srv
        .post("/api/debug/ftms-write", r#"{"client":64,"hex":"00"}"#)
        .json();
    assert_eq!(req["response"], "800001");
    assert_eq!(
        srv.post("/api/debug/ftms-write", r#"{"client":64,"hex":"07"}"#)
            .json()["status"],
        "04"
    );
    let tp = srv
        .post("/api/debug/ftms-write", r#"{"client":64,"hex":"05b400"}"#)
        .json();
    assert_eq!(
        (tp["response"].as_str(), tp["accepted"].as_bool()),
        (Some("800501"), Some(true))
    );

    let ctl = srv.get("/api/ftms/control").json();
    assert_eq!(ctl["control"]["controlOwner"], 64);
    assert_eq!(ctl["control"]["targetPower"], 180);
    assert_eq!(ctl["control"]["connected"], true);
    assert_eq!(ctl["control"]["lastCommand"]["command"], "setTargetPower");

    // ERG: the measured power approaches the target while the target stays separate.
    let measured = eventually("measured power to rise", || {
        let t = srv.get("/api/telemetry").json();
        (t["power"].as_f64()? > 20.0).then(|| t["power"].as_f64().unwrap())
    });
    assert!(measured <= 180.5);
    assert_eq!(srv.get("/api/telemetry").json()["targetPower"], 180);

    let text = srv.get("/api/events.txt");
    assert_eq!(text.status, 200);
    let text = String::from_utf8(text.body).unwrap();
    assert!(
        text.contains("Request Control") && text.contains("Set Target Power = 180 W"),
        "{text}"
    );
}

#[test]
fn clients_can_be_disconnected_from_the_api() {
    let srv = Server::start(false);
    srv.post(
        "/api/debug/ble",
        r#"{"event":"connect","client":64,"mtu":247}"#,
    );
    assert_eq!(srv.get("/api/ble").json()["clients"][0]["id"], 64);
    assert_eq!(srv.post("/api/ble/clients/64/disconnect", "").status, 200);
    eventually("the client to be dropped", || {
        srv.get("/api/ble").json()["clients"]
            .as_array()
            .is_some_and(|c| c.is_empty())
            .then_some(())
    });
    assert_eq!(srv.post("/api/ble/clients/64/disconnect", "").status, 404);
}

#[test]
fn settings_persist_to_disk() {
    let srv = Server::start(false);
    let r = srv.request("PUT", "/api/settings", r#"{"deviceName":"MyKickr"}"#);
    assert_eq!(r.status, 200);
    let file = srv.data_dir.join("settings.json");
    eventually("settings.json to be written", || {
        file.exists().then_some(())
    });
    assert!(std::fs::read_to_string(&file).unwrap().contains("MyKickr"));
    assert_eq!(
        srv.get("/api/state").json()["settings"]["deviceName"],
        "MyKickr"
    );
}

#[test]
fn provisioning_mode_reports_access_point_and_redirects() {
    let srv = Server::start(true);
    assert_eq!(srv.get("/api/system").json()["wifiMode"], "accessPoint");
    let r = srv.get("/generate_204");
    assert_eq!(r.status, 302);
    assert!(r.headers.contains("location: http://192.168.4.1/"));
    let scan = srv.get("/api/wifi/scan").json();
    assert!(scan["networks"].as_array().unwrap().len() >= 2);
    assert_eq!(
        srv.post("/api/wifi", r#"{"ssid":"x","password":"short"}"#)
            .status,
        422
    );
    assert_eq!(
        srv.post(
            "/api/wifi",
            r#"{"ssid":"home","password":"hunter2hunter2"}"#
        )
        .status,
        200
    );
}

// ---- WebSocket ----------------------------------------------------------------------------------

/// Reads one unmasked server frame; returns its text payload.
fn read_text_frame(s: &mut TcpStream) -> String {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).unwrap();
    assert_eq!(hdr[0] & 0x0F, 1, "text frame");
    assert_eq!(hdr[1] & 0x80, 0, "server frames are unmasked");
    let len = match hdr[1] & 0x7F {
        126 => {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => panic!("unexpectedly huge frame"),
        n => n as usize,
    };
    let mut payload = vec![0u8; len];
    s.read_exact(&mut payload).unwrap();
    String::from_utf8(payload).unwrap()
}

fn ws_connect(port: u16) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        b"GET /ws HTTP/1.1\r\nHost: test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).unwrap();
        head.push(b[0]);
    }
    let head = String::from_utf8(head).unwrap();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(
        head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "RFC 6455 example accept key: {head}"
    );
    s
}

fn next_frame_of_type(s: &mut TcpStream, ty: &str) -> serde_json::Value {
    for _ in 0..40 {
        let v: serde_json::Value = serde_json::from_str(&read_text_frame(s)).unwrap();
        if v["type"] == ty {
            return v;
        }
    }
    panic!("no {ty} frame within 40 frames");
}

#[test]
fn websocket_streams_state_telemetry_and_ftms_commands() {
    let srv = Server::start(false);
    let mut ws = ws_connect(srv.port);

    let hello: serde_json::Value = serde_json::from_str(&read_text_frame(&mut ws)).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["data"]["protocol"], 1);
    let state: serde_json::Value = serde_json::from_str(&read_text_frame(&mut ws)).unwrap();
    assert_eq!(state["type"], "state");
    assert_eq!(state["data"]["settings"]["hostname"], "fitness-simulator");

    // Telemetry ticks in on its own.
    let t = next_frame_of_type(&mut ws, "telemetry");
    assert!(t["data"]["power"].is_number() && t["timestamp"].is_number());

    // Changes made over REST appear on the stream without polling.
    srv.post("/api/state/power", r#"{"value":321}"#);
    let mut seen = false;
    for _ in 0..30 {
        let v = next_frame_of_type(&mut ws, "telemetry");
        if v["data"]["power"] == 321.0 {
            seen = true;
            break;
        }
    }
    assert!(seen, "power change never reached the stream");

    // FTMS commands are pushed as `ftms-command` events.
    srv.post("/api/debug/ftms-write", r#"{"client":9,"hex":"00"}"#);
    srv.post("/api/debug/ftms-write", r#"{"client":9,"hex":"05fa00"}"#);
    let mut cmd = next_frame_of_type(&mut ws, "ftms-command");
    if cmd["command"] == "requestControl" {
        cmd = next_frame_of_type(&mut ws, "ftms-command");
    }
    assert_eq!(cmd["command"], "setTargetPower");
    assert_eq!(cmd["value"], 250.0);
    assert_eq!(cmd["data"]["result"], "success");
}

#[test]
fn websocket_requires_an_upgrade_request() {
    let srv = Server::start(false);
    assert_eq!(srv.get("/ws").status, 400);
}
