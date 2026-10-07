//! The central simulation state shared by the BLE services and the REST/WebSocket layer.
//! Pure logic: callers feed it time via [`Simulator::tick`] and push hardware events (BLE
//! connections, Control Point writes) into it.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use serde::Serialize;

use crate::cps::{self, CrankModel};
use crate::event::{EventLog, Kind, Level};
use crate::ftms::{self, BikeSample, Packet};
use crate::generator::{Generator, GeneratorRuntime};
use crate::hrs::{self, BeatTracker};
use crate::metric::{Device, Metric};
use crate::rng::Rng;
use crate::settings::Settings;
use crate::trainer::{ClientId, ControlOutcome, MachineState, RoadParams, TrainerState};

/// Tick period the firmware should aim for.
pub const TICK_MS: u64 = 50;
/// How often each device sends a measurement notification.
pub fn notify_interval_ms(d: Device) -> u32 {
    match d {
        Device::Trainer | Device::PowerMeter => 250,
        Device::HeartRate => 1000,
    }
}

// ---- BLE bookkeeping --------------------------------------------------------------------------

/// GATT characteristics a client can subscribe to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Char {
    IndoorBikeData,
    MachineStatus,
    ControlPoint,
    TrainingStatus,
    HeartRateMeasurement,
    PowerMeasurement,
}

impl Char {
    pub const ALL: [Char; 6] = [
        Char::IndoorBikeData,
        Char::MachineStatus,
        Char::ControlPoint,
        Char::TrainingStatus,
        Char::HeartRateMeasurement,
        Char::PowerMeasurement,
    ];

    pub fn device(self) -> Device {
        match self {
            Char::IndoorBikeData
            | Char::MachineStatus
            | Char::ControlPoint
            | Char::TrainingStatus => Device::Trainer,
            Char::HeartRateMeasurement => Device::HeartRate,
            Char::PowerMeasurement => Device::PowerMeter,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Char::IndoorBikeData => "indoorBikeData",
            Char::MachineStatus => "machineStatus",
            Char::ControlPoint => "controlPoint",
            Char::TrainingStatus => "trainingStatus",
            Char::HeartRateMeasurement => "heartRateMeasurement",
            Char::PowerMeasurement => "cyclingPowerMeasurement",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Char::IndoorBikeData => "Indoor Bike Data",
            Char::MachineStatus => "Fitness Machine Status",
            Char::ControlPoint => "Fitness Machine Control Point",
            Char::TrainingStatus => "Training Status",
            Char::HeartRateMeasurement => "Heart Rate Measurement",
            Char::PowerMeasurement => "Cycling Power Measurement",
        }
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

#[derive(Clone, Debug)]
pub struct BleClient {
    pub id: ClientId,
    pub peer: String,
    pub connected_ms: u64,
    pub mtu: u16,
    pub rssi: Option<i8>,
    subs: u8,
}

impl BleClient {
    pub fn subscribed(&self, c: Char) -> bool {
        self.subs & c.bit() != 0
    }

    /// True if the client is subscribed to at least one characteristic of `d`.
    pub fn uses_device(&self, d: Device) -> bool {
        Char::ALL
            .iter()
            .any(|c| c.device() == d && self.subscribed(*c))
    }
}

#[derive(Clone, Debug, Default)]
pub struct WifiInfo {
    /// `station`, `accessPoint` or `none`.
    pub mode: &'static str,
    pub ssid: String,
    pub ip: String,
    pub rssi: Option<i32>,
}

/// Filled in by the platform layer; reported verbatim by `GET /api/system`.
#[derive(Clone, Debug, Default)]
pub struct PlatformInfo {
    pub chip: String,
    pub heap_free: u32,
    pub heap_total: u32,
    pub wifi: WifiInfo,
}

// ---- Output structures ------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Telemetry {
    pub speed: f32,
    pub cadence: f32,
    pub power: f32,
    pub heart_rate: f32,
    pub resistance: f32,
    /// FTMS ERG target, if one is active.
    pub target_power: Option<i16>,
    pub target_resistance: Option<u8>,
}

/// What currently determines a metric's value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// The metric's own generator.
    Generator,
    /// An FTMS power / resistance target set by the connected app.
    Trainer,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricInfo<'a> {
    pub label: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub generator: &'a Generator,
    pub source: Source,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsInfo<'a> {
    pub speed: MetricInfo<'a>,
    pub cadence: MetricInfo<'a>,
    pub power: MetricInfo<'a>,
    pub heart_rate: MetricInfo<'a>,
    pub resistance: MetricInfo<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub id: ClientId,
    pub peer: String,
    pub connected_ms: u64,
    pub mtu: u16,
    pub rssi: Option<i8>,
    pub subscribed: Vec<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    /// True while at least one client is subscribed to one of the device's characteristics.
    pub connected: bool,
    pub clients: usize,
    pub connected_since_ms: Option<u64>,
    pub mtu: Option<u16>,
    pub rssi: Option<i8>,
    pub subscribed: Vec<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicesStatus {
    pub trainer: DeviceStatus,
    pub heart_rate: DeviceStatus,
    pub power_meter: DeviceStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BleStatus {
    pub advertising: bool,
    pub devices: DevicesStatus,
    pub clients: Vec<ClientStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainerStatus {
    pub connected: bool,
    /// Connection handle of the client holding FTMS control.
    pub control_owner: Option<ClientId>,
    pub machine_state: MachineState,
    pub target_power: Option<i16>,
    pub target_resistance: Option<u8>,
    pub simulation: Option<RoadParams>,
    pub last_command: Option<crate::trainer::LastCommand>,
    pub commands_received: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeInfo {
    pub uptime_ms: u64,
    /// Add to an uptime to get Unix milliseconds; `None` until a browser synced the clock.
    pub wall_offset_ms: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub version: &'static str,
    pub chip: String,
    pub uptime_ms: u64,
    pub heap_free: u32,
    pub heap_total: u32,
    pub wifi_mode: &'static str,
    pub ssid: String,
    pub ip: String,
    pub rssi: Option<i32>,
    pub hostname: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot<'a> {
    pub time: TimeInfo,
    pub telemetry: Telemetry,
    pub metrics: MetricsInfo<'a>,
    pub ble: BleStatus,
    pub trainer: TrainerStatus,
    pub settings: &'a Settings,
    pub system: SystemStatus,
}

// ---- Simulator --------------------------------------------------------------------------------

struct Slot {
    generator: GeneratorRuntime,
    output: f32,
    source: Source,
}

pub struct Simulator {
    pub settings: Settings,
    pub log: EventLog,
    pub trainer: TrainerState,
    pub platform: PlatformInfo,
    /// Set by the BLE task.
    pub advertising: bool,
    /// Add to uptime to get Unix ms (set by `POST /api/time`).
    pub wall_offset_ms: Option<i64>,

    clients: Vec<BleClient>,
    slots: [Slot; 5],
    rng: Rng,
    now_ms: u64,
    crank: CrankModel,
    beats: BeatTracker,
    last_hr_packet_ms: u64,
    /// Recent Fitness Machine Status notifications `(sequence, payload)`, so every connection can
    /// forward the ones it has not sent yet.
    status_log: VecDeque<(u32, Vec<u8>)>,
    status_seq: u32,
    /// Clients the user asked to disconnect; the BLE task drops them and then reports back.
    disconnects: Vec<ClientId>,
    state_version: u32,
    name_version: u32,
}

impl Simulator {
    pub fn new(mut settings: Settings, seed: u32) -> Self {
        let _ = settings.sanitize();
        let slot = |g: Generator, v: f32| Slot {
            generator: GeneratorRuntime::new(g, 0, v),
            output: v,
            source: Source::Generator,
        };
        Self {
            settings,
            log: EventLog::default(),
            trainer: TrainerState::default(),
            platform: PlatformInfo::default(),
            advertising: false,
            wall_offset_ms: None,
            clients: Vec::new(),
            slots: [
                slot(Generator::Manual { value: 0.0 }, 0.0),
                slot(Generator::Manual { value: 0.0 }, 0.0),
                slot(Generator::Manual { value: 0.0 }, 0.0),
                slot(Generator::Manual { value: 80.0 }, 80.0),
                slot(Generator::Manual { value: 20.0 }, 20.0),
            ],
            rng: Rng::new(seed),
            now_ms: 0,
            crank: CrankModel::default(),
            beats: BeatTracker::default(),
            last_hr_packet_ms: 0,
            status_log: VecDeque::new(),
            status_seq: 0,
            disconnects: Vec::new(),
            state_version: 1,
            name_version: 1,
        }
    }

    // ---- time / bookkeeping -------------------------------------------------------------------

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Incremented whenever non-telemetry state changes (WebSocket clients then get a full state).
    pub fn state_version(&self) -> u32 {
        self.state_version
    }

    /// Incremented when the advertised name changes (the BLE task re-advertises).
    pub fn name_version(&self) -> u32 {
        self.name_version
    }

    pub fn bump(&mut self) {
        self.state_version = self.state_version.wrapping_add(1);
    }

    pub fn log(&mut self, level: Level, kind: Kind, msg: impl Into<String>) {
        self.log.record(self.now_ms, level, kind, msg);
        self.bump();
    }

    pub fn set_wall_clock(&mut self, unix_ms: i64) {
        self.wall_offset_ms = Some(unix_ms - self.now_ms as i64);
        self.bump();
    }

    // ---- the tick -----------------------------------------------------------------------------

    /// Advance the simulation to `now_ms` (device uptime). Call every [`TICK_MS`].
    ///
    /// Every metric follows its generator, except that FTMS targets override power (ERG) and
    /// resistance: the simulated trainer reaches its target instantly.
    pub fn tick(&mut self, now_ms: u64) {
        let dt = now_ms.saturating_sub(self.now_ms).min(1000) as f32 / 1000.0;
        self.now_ms = now_ms;

        for m in Metric::ALL {
            let slot = &mut self.slots[m.index()];
            let target = match m {
                Metric::Power => self.trainer.target_power.map(|w| w as f32),
                Metric::Resistance => self.trainer.target_resistance.map(|r| r as f32),
                _ => None,
            };
            let (value, source) = match target {
                Some(t) => (t, Source::Trainer),
                None => (
                    slot.generator.sample(now_ms, dt, &mut self.rng),
                    Source::Generator,
                ),
            };
            slot.output = m.clamp(value);
            slot.source = source;
        }
        self.crank.advance(now_ms, self.value(Metric::Cadence));
    }

    // ---- values -------------------------------------------------------------------------------

    pub fn value(&self, m: Metric) -> f32 {
        self.slots[m.index()].output
    }

    pub fn generator(&self, m: Metric) -> &Generator {
        &self.slots[m.index()].generator.config
    }

    pub fn telemetry(&self) -> Telemetry {
        let round = |m: Metric| crate::math::round_to(self.value(m), 1);
        Telemetry {
            speed: round(Metric::Speed),
            cadence: round(Metric::Cadence),
            power: round(Metric::Power),
            heart_rate: round(Metric::HeartRate),
            resistance: round(Metric::Resistance),
            target_power: self.trainer.target_power,
            target_resistance: self.trainer.target_resistance,
        }
    }

    /// Replaces a metric's generator. Switching modes is logged; slider drags are not.
    pub fn set_generator(&mut self, m: Metric, generator: Generator) -> Result<(), String> {
        generator.validate()?;
        let slot = &mut self.slots[m.index()];
        let quiet = matches!(
            (&slot.generator.config, &generator),
            (Generator::Manual { .. }, Generator::Manual { .. })
                | (Generator::Fixed { .. }, Generator::Fixed { .. })
        );
        let mode = generator.mode_name();
        let current = slot.output;
        slot.generator.set(generator, self.now_ms, current);
        if !quiet {
            self.log.record(
                self.now_ms,
                Level::Info,
                Kind::Api,
                alloc::format!("{}: mode set to {mode}", m.label()),
            );
        }
        self.bump();
        Ok(())
    }

    pub fn set_value(&mut self, m: Metric, value: f32) -> Result<(), String> {
        if !value.is_finite() {
            return Err("value must be a finite number".into());
        }
        let (lo, hi) = m.range();
        if value < lo || value > hi {
            return Err(alloc::format!(
                "{} must be within {lo}..={hi} {}",
                m.label(),
                m.unit()
            ));
        }
        self.set_generator(m, Generator::Manual { value })
    }

    // ---- BLE: clients -------------------------------------------------------------------------

    pub fn clients(&self) -> &[BleClient] {
        &self.clients
    }

    pub fn client_connected(&mut self, id: ClientId, peer: String, mtu: u16) {
        self.clients.retain(|c| c.id != id);
        self.log(
            Level::Info,
            Kind::Ble,
            alloc::format!("BLE client connected ({peer}, handle {id})"),
        );
        self.clients.push(BleClient {
            id,
            peer,
            connected_ms: self.now_ms,
            mtu,
            rssi: None,
            subs: 0,
        });
    }

    pub fn client_disconnected(&mut self, id: ClientId) {
        if let Some(pos) = self.clients.iter().position(|c| c.id == id) {
            let c = self.clients.remove(pos);
            self.log(
                Level::Info,
                Kind::Ble,
                alloc::format!("BLE client disconnected ({}, handle {id})", c.peer),
            );
        }
        self.disconnects.retain(|&d| d != id);
        self.trainer.on_disconnect(id, &mut self.log, self.now_ms);
        self.bump();
    }

    /// Asks the BLE layer to drop `id` (the web UI's Disconnect button).
    pub fn request_disconnect(&mut self, id: ClientId) -> Result<(), String> {
        let Some(peer) = self
            .clients
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.peer.clone())
        else {
            return Err(alloc::format!("no client with id {id}"));
        };
        if !self.disconnects.contains(&id) {
            self.disconnects.push(id);
            self.log(
                Level::Info,
                Kind::Ble,
                alloc::format!("Disconnect requested for {peer} (handle {id})"),
            );
        }
        Ok(())
    }

    /// True once if a disconnect was requested for `id`; the connection task polls this.
    pub fn take_disconnect(&mut self, id: ClientId) -> bool {
        let before = self.disconnects.len();
        self.disconnects.retain(|&d| d != id);
        self.disconnects.len() != before
    }

    /// Takes all pending disconnect requests (platforms without a real radio act on them directly).
    pub fn drain_disconnects(&mut self) -> Vec<ClientId> {
        core::mem::take(&mut self.disconnects)
    }

    pub fn client_update(&mut self, id: ClientId, mtu: Option<u16>, rssi: Option<i8>) {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == id) {
            let mut changed = false;
            if let Some(m) = mtu {
                changed |= c.mtu != m;
                c.mtu = m;
            }
            if rssi.is_some() {
                c.rssi = rssi;
            }
            if changed {
                self.bump();
            }
        }
    }

    pub fn client_subscription(&mut self, id: ClientId, ch: Char, on: bool) {
        let Some(c) = self.clients.iter_mut().find(|c| c.id == id) else {
            return;
        };
        let had = c.subscribed(ch);
        if on {
            c.subs |= ch.bit();
        } else {
            c.subs &= !ch.bit();
        }
        if had != on {
            let msg = alloc::format!(
                "{}: client {id} {} {}",
                ch.device().label(),
                if on {
                    "subscribed to"
                } else {
                    "unsubscribed from"
                },
                ch.label()
            );
            self.log(Level::Info, Kind::Ble, msg);
        }
    }

    pub fn device_connected(&self, d: Device) -> bool {
        self.clients.iter().any(|c| c.uses_device(d))
    }

    pub fn set_advertising(&mut self, on: bool) {
        if self.advertising != on {
            self.advertising = on;
            let msg = if on {
                "BLE advertising started"
            } else {
                "BLE advertising stopped"
            };
            self.log(Level::Info, Kind::Ble, msg);
        }
    }

    /// Sequence number of the newest Fitness Machine Status notification (0 = none yet).
    pub fn machine_status_seq(&self) -> u32 {
        self.status_seq
    }

    /// Fitness Machine Status notifications newer than `after`, oldest first.
    pub fn machine_status_since(&self, after: u32) -> Vec<(u32, Vec<u8>)> {
        self.status_log
            .iter()
            .filter(|(seq, _)| seq.wrapping_sub(after) as i32 > 0)
            .cloned()
            .collect()
    }

    // ---- BLE: packets -------------------------------------------------------------------------

    /// Indoor Bike Data notification sized for a link with `mtu`.
    pub fn bike_data_packet(&self, mtu: u16) -> Packet<{ ftms::BIKE_DATA_MAX_LEN }> {
        ftms::encode_indoor_bike_data(
            &BikeSample {
                speed_kmh: self.value(Metric::Speed),
                cadence_rpm: self.value(Metric::Cadence),
                resistance: self.value(Metric::Resistance),
                power_w: self.value(Metric::Power),
            },
            (mtu as usize).saturating_sub(3),
        )
    }

    /// Heart Rate Measurement including the RR intervals since the previous packet.
    pub fn heart_rate_packet(&mut self) -> Packet<{ hrs::MEASUREMENT_MAX_LEN }> {
        let dt = self.now_ms.saturating_sub(self.last_hr_packet_ms) as f32 / 1000.0;
        self.last_hr_packet_ms = self.now_ms;
        let hr = self.value(Metric::HeartRate);
        let (rr, n) = self.beats.advance(dt, hr);
        hrs::encode_measurement(hr.clamp(0.0, 255.0) as u8, &rr[..n])
    }

    pub fn power_packet(&self) -> [u8; cps::MEASUREMENT_LEN] {
        cps::encode_measurement(
            crate::math::round(self.value(Metric::Power)).clamp(-32768.0, 32767.0) as i16,
            self.crank.revs(),
            self.crank.last_event_1024(),
        )
    }

    // ---- FTMS ---------------------------------------------------------------------------------

    /// Handles a write to the FTMS Control Point and applies its effects to the simulation.
    pub fn ftms_control_write(&mut self, client: ClientId, data: &[u8]) -> ControlOutcome {
        let outcome = self
            .trainer
            .handle_write(client, data, &mut self.log, self.now_ms);
        if let Some(status) = &outcome.status {
            self.status_seq = self.status_seq.wrapping_add(1);
            if self.status_log.len() == 16 {
                self.status_log.pop_front();
            }
            self.status_log.push_back((self.status_seq, status.clone()));
        }
        self.bump();
        outcome
    }

    /// Clears all FTMS state (targets, simulation parameters, control ownership).
    pub fn reset_trainer(&mut self) {
        self.trainer.owner = None;
        self.trainer.reset();
        self.log(Level::Info, Kind::Ftms, "Trainer control state cleared");
    }

    // ---- settings -----------------------------------------------------------------------------

    /// Validates and applies new settings.
    pub fn apply_settings(&mut self, mut new: Settings) -> Result<(), String> {
        new.sanitize()?;
        if new.device_name != self.settings.device_name {
            self.name_version = self.name_version.wrapping_add(1);
        }
        let hostname_changed = new.hostname != self.settings.hostname;
        self.settings = new;
        let msg = if hostname_changed {
            "Settings updated (hostname change applies after reboot)"
        } else {
            "Settings updated"
        };
        self.log(Level::Info, Kind::System, msg);
        Ok(())
    }

    // ---- snapshot -----------------------------------------------------------------------------

    fn device_status(&self, d: Device) -> DeviceStatus {
        let users: Vec<&BleClient> = self.clients.iter().filter(|c| c.uses_device(d)).collect();
        let subscribed = Char::ALL
            .iter()
            .filter(|c| c.device() == d && users.iter().any(|u| u.subscribed(**c)))
            .map(|c| c.name())
            .collect();
        DeviceStatus {
            connected: !users.is_empty(),
            clients: users.len(),
            connected_since_ms: users.iter().map(|c| c.connected_ms).min(),
            mtu: users.first().map(|c| c.mtu),
            rssi: users.first().and_then(|c| c.rssi),
            subscribed,
        }
    }

    pub fn ble_status(&self) -> BleStatus {
        BleStatus {
            advertising: self.advertising,
            devices: DevicesStatus {
                trainer: self.device_status(Device::Trainer),
                heart_rate: self.device_status(Device::HeartRate),
                power_meter: self.device_status(Device::PowerMeter),
            },
            clients: self
                .clients
                .iter()
                .map(|c| ClientStatus {
                    id: c.id,
                    peer: c.peer.clone(),
                    connected_ms: c.connected_ms,
                    mtu: c.mtu,
                    rssi: c.rssi,
                    subscribed: Char::ALL
                        .iter()
                        .filter(|ch| c.subscribed(**ch))
                        .map(|ch| ch.name())
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn trainer_status(&self) -> TrainerStatus {
        TrainerStatus {
            connected: self.device_connected(Device::Trainer),
            control_owner: self.trainer.owner,
            machine_state: self.trainer.machine,
            target_power: self.trainer.target_power,
            target_resistance: self.trainer.target_resistance,
            simulation: self.trainer.road(),
            last_command: self.trainer.last_command.clone(),
            commands_received: self.trainer.commands_received,
        }
    }

    pub fn system_status(&self) -> SystemStatus {
        SystemStatus {
            version: crate::VERSION,
            chip: self.platform.chip.clone(),
            uptime_ms: self.now_ms,
            heap_free: self.platform.heap_free,
            heap_total: self.platform.heap_total,
            wifi_mode: if self.platform.wifi.mode.is_empty() {
                "none"
            } else {
                self.platform.wifi.mode
            },
            ssid: self.platform.wifi.ssid.clone(),
            ip: self.platform.wifi.ip.clone(),
            rssi: self.platform.wifi.rssi,
            hostname: self.settings.hostname.clone(),
        }
    }

    pub fn snapshot(&self) -> Snapshot<'_> {
        let info = |m: Metric| {
            let (min, max) = m.range();
            let slot = &self.slots[m.index()];
            MetricInfo {
                label: m.label(),
                unit: m.unit(),
                min,
                max,
                generator: &slot.generator.config,
                source: slot.source,
            }
        };
        Snapshot {
            time: TimeInfo {
                uptime_ms: self.now_ms,
                wall_offset_ms: self.wall_offset_ms,
            },
            telemetry: self.telemetry(),
            metrics: MetricsInfo {
                speed: info(Metric::Speed),
                cadence: info(Metric::Cadence),
                power: info(Metric::Power),
                heart_rate: info(Metric::HeartRate),
                resistance: info(Metric::Resistance),
            },
            ble: self.ble_status(),
            trainer: self.trainer_status(),
            settings: &self.settings,
            system: self.system_status(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::Waveform;

    fn sim() -> Simulator {
        Simulator::new(Settings::default(), 1234)
    }

    /// Runs the simulator for `secs` at the firmware tick rate.
    fn run(sim: &mut Simulator, secs: f32) {
        let steps = (secs * 1000.0 / TICK_MS as f32) as u64;
        let start = sim.now_ms();
        for i in 1..=steps {
            sim.tick(start + i * TICK_MS);
        }
    }

    #[test]
    fn disconnect_requests_are_queued_once_and_cleared() {
        let mut s = sim();
        assert!(s.request_disconnect(7).is_err(), "unknown client");
        s.client_connected(7, "AA:BB".into(), 247);
        s.request_disconnect(7).unwrap();
        s.request_disconnect(7).unwrap();
        assert!(s.take_disconnect(7));
        assert!(!s.take_disconnect(7), "reported once");
        s.request_disconnect(7).unwrap();
        s.client_disconnected(7);
        assert!(
            s.drain_disconnects().is_empty(),
            "a client that is gone leaves no request behind"
        );
    }

    #[test]
    fn manual_values_reach_telemetry() {
        let mut s = sim();
        s.set_value(Metric::Power, 250.0).unwrap();
        s.set_value(Metric::Cadence, 92.0).unwrap();
        s.set_value(Metric::HeartRate, 154.0).unwrap();
        run(&mut s, 0.1);
        let t = s.telemetry();
        assert_eq!((t.power, t.cadence, t.heart_rate), (250.0, 92.0, 154.0));
    }

    #[test]
    fn rejects_out_of_range_and_nonsense() {
        let mut s = sim();
        assert!(s.set_value(Metric::HeartRate, 999.0).is_err());
        assert!(s.set_value(Metric::Power, f32::NAN).is_err());
    }

    #[test]
    fn ramp_generator_drives_power() {
        let mut s = sim();
        s.set_generator(
            Metric::Power,
            Generator::Ramp {
                start: 100.0,
                end: 400.0,
                duration_s: 60.0,
                repeat: false,
            },
        )
        .unwrap();
        run(&mut s, 30.0);
        assert!((s.value(Metric::Power) - 250.0).abs() < 3.0);
        run(&mut s, 40.0);
        assert_eq!(s.value(Metric::Power), 400.0);
    }

    #[test]
    fn oscillating_heart_rate_stays_in_band() {
        let mut s = sim();
        s.set_generator(
            Metric::HeartRate,
            Generator::Oscillation {
                center: 150.0,
                amplitude: 10.0,
                period_s: 20.0,
                waveform: Waveform::Sine,
            },
        )
        .unwrap();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for _ in 0..400 {
            run(&mut s, 0.05);
            lo = lo.min(s.value(Metric::HeartRate));
            hi = hi.max(s.value(Metric::HeartRate));
        }
        assert!(
            (lo - 140.0).abs() < 0.5 && (hi - 160.0).abs() < 0.5,
            "{lo}..{hi}"
        );
    }

    #[test]
    fn ftms_targets_override_power_and_resistance() {
        let mut s = sim();
        s.set_value(Metric::Power, 100.0).unwrap();
        s.set_value(Metric::Resistance, 20.0).unwrap();
        run(&mut s, 0.2);
        assert_eq!(s.value(Metric::Power), 100.0);
        s.ftms_control_write(1, &[0x00]);
        s.ftms_control_write(1, &[0x05, 250, 0]);
        run(&mut s, 0.1);
        assert_eq!(
            s.value(Metric::Power),
            250.0,
            "measured power equals the target"
        );
        assert_eq!(s.telemetry().target_power, Some(250));
        assert_eq!(s.snapshot().metrics.power.source, Source::Trainer);
        // A resistance target replaces the power target and hands power back to the generator.
        s.ftms_control_write(1, &[0x04, 30]);
        run(&mut s, 0.1);
        assert_eq!(s.value(Metric::Power), 100.0);
        assert_eq!(s.value(Metric::Resistance), 30.0);
        assert_eq!(s.snapshot().metrics.power.source, Source::Generator);
    }

    #[test]
    fn simulation_params_are_reported_without_float_noise() {
        let mut s = sim();
        s.ftms_control_write(1, &[0x00]);
        // grade +7.4 %, wind -1.2 m/s
        s.ftms_control_write(1, &[0x11, 0x18, 0xFB, 0xE4, 0x02, 40, 51]);
        let st = s.trainer_status();
        let r = st.simulation.unwrap();
        assert_eq!(
            (r.grade, r.wind_speed, r.crr, r.cw),
            (7.4, -1.256, 0.004, 0.51)
        );
        let json = serde_json::to_string(&st).unwrap();
        assert!(json.contains(r#""grade":7.4,"#), "no float noise: {json}");
    }

    #[test]
    fn ble_clients_and_subscriptions() {
        let mut s = sim();
        s.tick(1000);
        s.client_connected(64, "AA:BB:CC:DD:EE:FF".into(), 23);
        assert!(!s.device_connected(Device::Trainer), "not subscribed yet");
        s.client_subscription(64, Char::IndoorBikeData, true);
        s.client_subscription(64, Char::ControlPoint, true);
        s.client_update(64, Some(247), Some(-60));
        let st = s.ble_status();
        assert!(st.devices.trainer.connected);
        assert_eq!(st.devices.trainer.mtu, Some(247));
        assert_eq!(st.devices.trainer.rssi, Some(-60));
        assert_eq!(
            st.devices.trainer.subscribed,
            ["indoorBikeData", "controlPoint"]
        );
        assert_eq!(st.devices.trainer.connected_since_ms, Some(1000));
        assert!(!st.devices.heart_rate.connected);

        s.ftms_control_write(64, &[0x00]);
        assert_eq!(s.trainer.owner, Some(64));
        s.client_disconnected(64);
        assert_eq!(
            s.trainer.owner, None,
            "control released with the connection"
        );
        assert!(!s.device_connected(Device::Trainer));
    }

    #[test]
    fn packets_reflect_state_and_mtu() {
        let mut s = sim();
        s.set_value(Metric::Power, 245.0).unwrap();
        s.set_value(Metric::Cadence, 92.0).unwrap();
        s.set_value(Metric::HeartRate, 154.0).unwrap();
        run(&mut s, 2.0);
        let big = s.bike_data_packet(247);
        let small = s.bike_data_packet(23);
        assert_eq!((big.len, small.len), (10, 10));
        assert_eq!(i16::from_le_bytes([big.bytes[8], big.bytes[9]]), 245);
        assert_eq!(s.heart_rate_packet().as_slice()[1], 154);
        let p = s.power_packet();
        assert_eq!(i16::from_le_bytes([p[2], p[3]]), 245);
        assert!(
            u16::from_le_bytes([p[4], p[5]]) >= 2,
            "crank revolutions accumulate"
        );
    }

    #[test]
    fn machine_status_queue_feeds_every_connection() {
        let mut s = sim();
        s.ftms_control_write(1, &[0x00]); // no status for Request Control
        assert_eq!(s.machine_status_seq(), 0);
        s.ftms_control_write(1, &[0x07]);
        s.ftms_control_write(1, &[0x05, 200, 0]);
        let all = s.machine_status_since(0);
        assert_eq!(
            all.iter().map(|(_, m)| m.as_slice()).collect::<Vec<_>>(),
            [&[0x04][..], &[0x08, 200, 0][..]]
        );
        assert_eq!(s.machine_status_since(all[0].0).len(), 1);
        assert!(s.machine_status_since(s.machine_status_seq()).is_empty());
        for _ in 0..40 {
            s.ftms_control_write(1, &[0x07]);
        }
        assert_eq!(s.machine_status_since(0).len(), 16, "the queue is bounded");
    }

    #[test]
    fn settings_changes() {
        let mut s = sim();
        let v = s.name_version();
        let mut new = s.settings.clone();
        new.hostname = "desk".into();
        s.apply_settings(new).unwrap();
        assert_eq!(s.name_version(), v, "only a name change re-advertises");
        let mut new = s.settings.clone();
        new.device_name = "Kickr".into();
        s.apply_settings(new).unwrap();
        assert_eq!(s.name_version(), v + 1);
        let mut bad = s.settings.clone();
        bad.hostname = "NOT valid".into();
        assert!(s.apply_settings(bad).is_err());
    }

    #[test]
    fn snapshot_serialises() {
        let mut s = sim();
        run(&mut s, 0.1);
        let json = serde_json::to_string(&s.snapshot()).unwrap();
        for key in [
            "telemetry",
            "metrics",
            "ble",
            "trainer",
            "settings",
            "system",
            "heartRate",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        assert!(!json.contains("faults") && !json.contains("scenario"));
    }

    #[test]
    fn wall_clock_sync() {
        let mut s = sim();
        s.tick(5_000);
        s.set_wall_clock(1_700_000_000_000);
        assert_eq!(s.wall_offset_ms, Some(1_700_000_000_000 - 5_000));
    }
}
