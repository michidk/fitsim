//! Chronological event log (BLE connections, FTMS commands, API changes and system events).
//!
//! Timestamps are device uptime in milliseconds. If the browser has synced the wall clock
//! (`POST /api/time`) the text rendering uses UTC `HH:MM:SS`, otherwise `T+HH:MM:SS`.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use serde::Serialize;

pub const DEFAULT_CAPACITY: usize = 300;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Ble,
    Ftms,
    System,
    Api,
}

/// Structured payload attached to FTMS Control Point events (also sent as a `ftms-command`
/// WebSocket frame).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FtmsCommand {
    /// camelCase command name, e.g. `setTargetPower`.
    pub command: &'static str,
    /// Human readable op code name, e.g. `Set Target Power`.
    pub name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<&'static str>,
    /// `success`, `notSupported`, `invalidParameter`, `operationFailed` or `controlNotPermitted`.
    pub result: &'static str,
    /// BLE connection handle of the client that wrote the command.
    pub client: u16,
    /// Raw Control Point bytes as hex.
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub seq: u32,
    /// Device uptime in milliseconds.
    pub t: u64,
    pub level: Level,
    pub kind: Kind,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ftms: Option<FtmsCommand>,
}

#[derive(Debug)]
pub struct EventLog {
    events: VecDeque<Event>,
    capacity: usize,
    next_seq: u32,
    /// Bumped on `clear()` so streaming clients know to drop their copy.
    epoch: u32,
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl EventLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(capacity.min(64)),
            capacity: capacity.max(1),
            next_seq: 1,
            epoch: 0,
        }
    }

    pub fn push(&mut self, event: Event) -> u32 {
        if self.events.len() == self.capacity {
            self.events.pop_front();
        }
        let seq = event.seq;
        self.events.push_back(event);
        seq
    }

    /// Records an event and returns its sequence number.
    pub fn record(&mut self, t: u64, level: Level, kind: Kind, message: impl Into<String>) -> u32 {
        self.record_full(t, level, kind, message.into(), Vec::new(), None)
    }

    pub fn record_full(
        &mut self,
        t: u64,
        level: Level,
        kind: Kind,
        message: String,
        detail: Vec<String>,
        ftms: Option<FtmsCommand>,
    ) -> u32 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        self.push(Event {
            seq,
            t,
            level,
            kind,
            message,
            detail,
            ftms,
        })
    }

    /// Events with `seq > after`, oldest first.
    pub fn since(&self, after: u32) -> impl Iterator<Item = &Event> {
        self.events.iter().filter(move |e| e.seq > after)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    /// Most recently recorded event, e.g. to attach detail lines to it.
    pub fn last_mut(&mut self) -> Option<&mut Event> {
        self.events.back_mut()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn last_seq(&self) -> u32 {
        self.next_seq.wrapping_sub(1)
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Plain-text rendering for download (`GET /api/events.txt`).
    pub fn to_text(&self, wall_offset_ms: Option<i64>) -> String {
        let mut out = String::new();
        for e in &self.events {
            write_event_text(&mut out, e, wall_offset_ms);
        }
        out
    }
}

pub fn write_event_text(out: &mut String, e: &Event, wall_offset_ms: Option<i64>) {
    let _ = writeln!(out, "{} {}", format_time(e.t, wall_offset_ms), e.message);
    for line in &e.detail {
        let _ = writeln!(out, "           {line}");
    }
}

/// `HH:MM:SS` (UTC) when the wall clock is known, `T+HH:MM:SS` otherwise.
pub fn format_time(t_ms: u64, wall_offset_ms: Option<i64>) -> String {
    let mut s = String::new();
    match wall_offset_ms {
        Some(off) => {
            let wall = (t_ms as i64 + off).max(0) as u64 / 1000;
            let _ = write!(
                s,
                "{:02}:{:02}:{:02}",
                (wall / 3600) % 24,
                (wall / 60) % 60,
                wall % 60
            );
        }
        None => {
            let secs = t_ms / 1000;
            let _ = write!(
                s,
                "T+{:02}:{:02}:{:02}",
                secs / 3600,
                (secs / 60) % 60,
                secs % 60
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_drops_oldest() {
        let mut log = EventLog::new(3);
        for i in 0..5 {
            log.record(i, Level::Info, Kind::System, alloc::format!("e{i}"));
        }
        let msgs: Vec<_> = log.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(msgs, ["e2", "e3", "e4"]);
        assert_eq!(log.last_seq(), 5);
        assert_eq!(log.since(4).count(), 1);
    }

    #[test]
    fn clear_bumps_epoch_but_not_sequence() {
        let mut log = EventLog::default();
        log.record(0, Level::Info, Kind::Ble, "a");
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.epoch(), 1);
        let seq = log.record(1, Level::Info, Kind::Ble, "b");
        assert_eq!(seq, 2);
    }

    #[test]
    fn text_rendering_matches_the_spec_example() {
        let mut log = EventLog::default();
        // wall clock 17:42:00 at uptime 0
        let off = Some(((17 * 3600 + 42 * 60) * 1000) as i64);
        log.record(1_000, Level::Info, Kind::Ble, "BLE client connected");
        log.record_full(
            61_000,
            Level::Info,
            Kind::Ftms,
            "Set Indoor Bike Simulation Parameters".into(),
            alloc::vec!["Wind speed: 0 m/s".into(), "Grade: 4.0 %".into()],
            None,
        );
        let text = log.to_text(off);
        assert_eq!(
            text,
            "17:42:01 BLE client connected\n17:43:01 Set Indoor Bike Simulation Parameters\n           Wind speed: 0 m/s\n           Grade: 4.0 %\n"
        );
        assert!(
            log.to_text(None)
                .starts_with("T+00:00:01 BLE client connected")
        );
    }

    #[test]
    fn serialises_camel_case_and_skips_empty() {
        let mut log = EventLog::default();
        log.record(5, Level::Warn, Kind::System, "x");
        let json = serde_json::to_string(log.iter().next().unwrap()).unwrap();
        assert_eq!(
            json,
            r#"{"seq":1,"t":5,"level":"warn","kind":"system","message":"x"}"#
        );
    }
}
