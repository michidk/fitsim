//! WebSocket framing (RFC 6455) and the JSON event stream sent to dashboard clients.
//!
//! Event format (one JSON object per text frame):
//!
//! | `type`           | payload                                                                   |
//! |------------------|---------------------------------------------------------------------------|
//! | `hello`          | `data: {version, protocol, lastEventSeq}` – first frame after connecting  |
//! | `state`          | `data:` full snapshot, whenever non-telemetry state changed               |
//! | `telemetry`      | `data:` live values, ~5 Hz                                                |
//! | `log`            | `data:` one event log entry (`seq`, `t`, `level`, `kind`, `message`, …)   |
//! | `ftms-command`   | `command`, `value`, `data:` command record – a Control Point write        |
//! | `ble-state`      | `data:` BLE devices and clients                                           |
//! | `log-cleared`    | the event log was cleared                                                 |
//!
//! Every frame carries `timestamp` (device uptime in ms).

use alloc::string::String;
use alloc::vec::Vec;
use serde::Serialize;

use crate::event::{Event, Kind};
use crate::sim::Simulator;

pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Poll period for building frames.
pub const POLL_MS: u64 = 200;
pub const PROTOCOL_VERSION: u32 = 1;
/// Largest client frame payload we accept.
pub const MAX_CLIENT_PAYLOAD: usize = 512;

pub mod opcode {
    pub const CONTINUATION: u8 = 0x0;
    pub const TEXT: u8 = 0x1;
    pub const BINARY: u8 = 0x2;
    pub const CLOSE: u8 = 0x8;
    pub const PING: u8 = 0x9;
    pub const PONG: u8 = 0xA;
}

// ---- Handshake --------------------------------------------------------------------------------

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(T[(b[0] >> 2) as usize] as char);
        out.push(T[(((b[0] & 3) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(((b[1] & 15) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(b[2] & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// `Sec-WebSocket-Accept` value for a client's `Sec-WebSocket-Key`.
pub fn accept_key(client_key: &str) -> String {
    let mut sha = sha1_smol::Sha1::new();
    sha.update(client_key.as_bytes());
    sha.update(WS_GUID.as_bytes());
    base64(&sha.digest().bytes())
}

// ---- Framing ----------------------------------------------------------------------------------

/// Appends one unmasked, unfragmented server frame.
pub fn encode_frame(opcode: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= 0xFFFF => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
}

pub fn text_frame(text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(text.len() + 4);
    encode_frame(opcode::TEXT, text.as_bytes(), &mut v);
    v
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub opcode: u8,
    pub payload: Vec<u8>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WsError {
    /// Client frames must be masked (RFC 6455 §5.3).
    Unmasked,
    /// Fragmented messages are not supported.
    Fragmented,
    TooLarge,
}

/// Decodes one client frame from the front of `buf`. Returns the frame and the bytes consumed,
/// or `Ok(None)` if more data is needed.
pub fn decode_frame(buf: &[u8]) -> Result<Option<(Frame, usize)>, WsError> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let fin = buf[0] & 0x80 != 0;
    let op = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    let (len, mut pos) = match buf[1] & 0x7F {
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4)
        }
        127 => return Err(WsError::TooLarge),
        n => (n as usize, 2),
    };
    if !fin || op == opcode::CONTINUATION {
        return Err(WsError::Fragmented);
    }
    if !masked {
        return Err(WsError::Unmasked);
    }
    if len > MAX_CLIENT_PAYLOAD {
        return Err(WsError::TooLarge);
    }
    if buf.len() < pos + 4 + len {
        return Ok(None);
    }
    let mask = [buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]];
    pos += 4;
    let payload = buf[pos..pos + len]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i % 4])
        .collect();
    Ok(Some((
        Frame {
            opcode: op,
            payload,
        },
        pos + len,
    )))
}

// ---- Event stream -----------------------------------------------------------------------------

/// Per-connection position in the event stream.
#[derive(Debug, Default)]
pub struct Cursor {
    started: bool,
    last_event: u32,
    epoch: u32,
    state_version: u32,
}

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    #[serde(rename = "type")]
    kind: &'a str,
    timestamp: u64,
    data: T,
}

fn envelope<T: Serialize>(kind: &str, timestamp: u64, data: T) -> String {
    serde_json::to_string(&Envelope {
        kind,
        timestamp,
        data,
    })
    .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    version: &'static str,
    protocol: u32,
    last_event_seq: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FtmsFrame<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    timestamp: u64,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<f32>,
    message: &'a str,
    data: &'a crate::event::FtmsCommand,
}

/// Builds the frames a connected client should receive now, and advances its cursor.
/// Call every [`POLL_MS`]; pass `telemetry = true` to include a telemetry frame.
pub fn poll_frames(sim: &Simulator, cur: &mut Cursor, telemetry: bool) -> Vec<String> {
    let now = sim.now_ms();
    let mut out = Vec::new();

    if !cur.started {
        cur.started = true;
        cur.last_event = sim.log.last_seq();
        cur.epoch = sim.log.epoch();
        cur.state_version = sim.state_version();
        out.push(envelope(
            "hello",
            now,
            Hello {
                version: crate::VERSION,
                protocol: PROTOCOL_VERSION,
                last_event_seq: cur.last_event,
            },
        ));
        out.push(envelope("state", now, sim.snapshot()));
    } else {
        if cur.epoch != sim.log.epoch() {
            cur.epoch = sim.log.epoch();
            cur.last_event = 0;
            out.push(envelope("log-cleared", now, ()));
        }
        let new_events: Vec<&Event> = sim.log.since(cur.last_event).collect();
        for e in &new_events {
            out.push(envelope("log", e.t, e));
            match (e.kind, &e.ftms) {
                (Kind::Ftms, Some(cmd)) => out.push(
                    serde_json::to_string(&FtmsFrame {
                        kind: "ftms-command",
                        timestamp: e.t,
                        command: cmd.command,
                        value: cmd.value,
                        message: &e.message,
                        data: cmd,
                    })
                    .unwrap_or_default(),
                ),
                (Kind::Ble, _) => out.push(envelope("ble-state", e.t, sim.ble_status())),
                _ => {}
            }
        }
        if let Some(last) = new_events.last() {
            cur.last_event = last.seq;
        }
        if cur.state_version != sim.state_version() {
            cur.state_version = sim.state_version();
            out.push(envelope("state", now, sim.snapshot()));
        }
    }

    if telemetry {
        out.push(envelope("telemetry", now, sim.telemetry()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use alloc::string::ToString;

    #[test]
    fn rfc6455_example_handshake() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn base64_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encodes_small_medium_and_large_frames() {
        let mut v = Vec::new();
        encode_frame(opcode::TEXT, b"hi", &mut v);
        assert_eq!(v, [0x81, 2, b'h', b'i']);
        let mut v = Vec::new();
        encode_frame(opcode::TEXT, &[b'a'; 300], &mut v);
        assert_eq!(&v[..4], [0x81, 126, 0x01, 0x2C]);
        assert_eq!(v.len(), 4 + 300);
        let mut v = Vec::new();
        encode_frame(opcode::TEXT, &[b'a'; 70_000], &mut v);
        assert_eq!(&v[..2], [0x81, 127]);
        assert_eq!(u64::from_be_bytes(v[2..10].try_into().unwrap()), 70_000);
    }

    #[test]
    fn decodes_masked_client_frames() {
        // RFC 6455 §5.7: masked "Hello"
        let raw = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let (f, used) = decode_frame(&raw).unwrap().unwrap();
        assert_eq!(
            (f.opcode, f.payload.as_slice(), used),
            (opcode::TEXT, &b"Hello"[..], raw.len())
        );
        // partial
        assert_eq!(decode_frame(&raw[..8]), Ok(None));
        assert_eq!(decode_frame(&raw[..1]), Ok(None));
    }

    #[test]
    fn rejects_unmasked_fragmented_and_huge_frames() {
        assert_eq!(
            decode_frame(&[0x81, 0x02, b'h', b'i']),
            Err(WsError::Unmasked)
        );
        assert_eq!(
            decode_frame(&[0x01, 0x80, 0, 0, 0, 0]),
            Err(WsError::Fragmented)
        );
        assert_eq!(
            decode_frame(&[0x81, 0xFE, 0x10, 0x00]),
            Err(WsError::TooLarge)
        );
        assert_eq!(decode_frame(&[0x81, 0xFF]), Err(WsError::TooLarge));
    }

    fn typed(frames: &[String]) -> Vec<String> {
        frames
            .iter()
            .map(|f| {
                serde_json::from_str::<serde_json::Value>(f).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn stream_starts_with_hello_and_state() {
        let sim = Simulator::new(Settings::default(), 1);
        let mut cur = Cursor::default();
        let frames = poll_frames(&sim, &mut cur, true);
        assert_eq!(typed(&frames), ["hello", "state", "telemetry"]);
        // Nothing changed: only telemetry.
        assert_eq!(typed(&poll_frames(&sim, &mut cur, true)), ["telemetry"]);
        assert!(poll_frames(&sim, &mut cur, false).is_empty());
    }

    #[test]
    fn stream_carries_ftms_commands_in_the_documented_shape() {
        let mut sim = Simulator::new(Settings::default(), 1);
        let mut cur = Cursor::default();
        poll_frames(&sim, &mut cur, false);
        sim.ftms_control_write(7, &[0x00]);
        sim.ftms_control_write(7, &[0x05, 0xFA, 0x00]);
        let frames = poll_frames(&sim, &mut cur, false);
        assert_eq!(
            typed(&frames),
            ["log", "ftms-command", "log", "log", "ftms-command", "state"]
        );
        let cmd: serde_json::Value = serde_json::from_str(&frames[4]).unwrap();
        assert_eq!(cmd["command"], "setTargetPower");
        assert_eq!(cmd["value"], 250.0);
        assert_eq!(cmd["data"]["result"], "success");
        assert_eq!(cmd["data"]["client"], 7);
    }

    #[test]
    fn stream_reports_ble_changes_and_log_clear() {
        let mut sim = Simulator::new(Settings::default(), 1);
        let mut cur = Cursor::default();
        poll_frames(&sim, &mut cur, false);
        sim.client_connected(1, "AA:BB".into(), 23);
        let kinds = typed(&poll_frames(&sim, &mut cur, false));
        for expected in ["ble-state", "state"] {
            assert!(
                kinds.iter().any(|k| k == expected),
                "{expected} missing in {kinds:?}"
            );
        }
        sim.log.clear();
        assert_eq!(typed(&poll_frames(&sim, &mut cur, false))[0], "log-cleared");
    }

    #[test]
    fn telemetry_matches_the_documented_example() {
        let mut sim = Simulator::new(Settings::default(), 1);
        sim.set_value(crate::metric::Metric::Power, 248.0).unwrap();
        sim.tick(100);
        let mut cur = Cursor::default();
        let frames = poll_frames(&sim, &mut cur, true);
        let t: serde_json::Value = serde_json::from_str(frames.last().unwrap()).unwrap();
        assert_eq!(t["type"], "telemetry");
        assert_eq!(t["timestamp"], 100);
        assert_eq!(t["data"]["power"], 248.0);
        for k in ["cadence", "heartRate", "speed", "resistance"] {
            assert!(t["data"][k].is_number(), "{k}");
        }
    }
}
