//! FTMS trainer behaviour behind the Control Point: control ownership, targets, simulation
//! parameters. Pure state machine; BLE I/O lives elsewhere.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use serde::Serialize;

use crate::event::{EventLog, FtmsCommand, Kind, Level};
use crate::ftms::{self, ControlRequest, IndoorBikeSimulation, ParseError, op, result, status};

/// BLE connection handle of the writing client.
pub type ClientId = u16;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MachineState {
    #[default]
    Stopped,
    Started,
    Paused,
}

/// Everything the BLE layer needs to answer a Control Point write.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlOutcome {
    /// Control Point indication payload.
    pub response: [u8; 3],
    /// Fitness Machine Status notification to send (to all subscribers) on success.
    pub status: Option<Vec<u8>>,
    pub accepted: bool,
}

/// Road simulation parameters as sent by "Set Indoor Bike Simulation Parameters".
#[derive(Copy, Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoadParams {
    /// Head wind in m/s (negative = tail wind).
    pub wind_speed: f32,
    /// Grade in percent.
    pub grade: f32,
    /// Coefficient of rolling resistance.
    pub crr: f32,
    /// Wind resistance coefficient in kg/m.
    pub cw: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastCommand {
    pub t: u64,
    pub command: &'static str,
    pub name: &'static str,
    pub result: &'static str,
    pub client: ClientId,
    /// Human readable parameters, e.g. `250 W`.
    pub summary: String,
}

#[derive(Clone, Debug, Default)]
pub struct TrainerState {
    pub owner: Option<ClientId>,
    pub machine: MachineState,
    /// ERG target in watts.
    pub target_power: Option<i16>,
    /// Target resistance in percent (`raw / 1`, see [`ftms::RESISTANCE_RAW_MAX`]).
    pub target_resistance: Option<u8>,
    /// Road simulation parameters, once the client has sent any.
    pub simulation: Option<IndoorBikeSimulation>,
    pub last_command: Option<LastCommand>,
    pub commands_received: u32,
}

impl TrainerState {
    /// The road parameters the client sent, at protocol resolution (raw values times 0.01 etc.
    /// leave float noise such as 7.3999996).
    pub fn road(&self) -> Option<RoadParams> {
        self.simulation.map(|s| RoadParams {
            wind_speed: crate::math::round_to(s.wind_speed_ms(), 3),
            grade: crate::math::round_to(s.grade_percent(), 2),
            crr: crate::math::round_to(s.crr(), 4),
            cw: crate::math::round_to(s.cw(), 2),
        })
    }

    pub fn training_status(&self) -> ftms::TrainingStatus {
        if self.target_power.is_some() {
            ftms::TrainingStatus::WattControl
        } else if self.machine == MachineState::Started {
            ftms::TrainingStatus::ManualMode
        } else {
            ftms::TrainingStatus::Idle
        }
    }

    fn clear_targets(&mut self) {
        self.target_power = None;
        self.target_resistance = None;
    }

    /// Releases control if `client` owned it.
    pub fn on_disconnect(&mut self, client: ClientId, log: &mut EventLog, now_ms: u64) {
        if self.owner == Some(client) {
            self.owner = None;
            log.record(
                now_ms,
                Level::Info,
                Kind::Ftms,
                "Control released (client disconnected)",
            );
        }
    }

    pub fn reset(&mut self) {
        *self = Self {
            commands_received: self.commands_received,
            last_command: self.last_command.take(),
            ..Self::default()
        };
    }

    /// Processes one Control Point write.
    pub fn handle_write(
        &mut self,
        client: ClientId,
        data: &[u8],
        log: &mut EventLog,
        now_ms: u64,
    ) -> ControlOutcome {
        self.commands_received += 1;
        let raw = hex(data);
        let request = match ftms::parse_control_point(data) {
            Ok(r) => r,
            Err(ParseError::Empty) => {
                log.record(
                    now_ms,
                    Level::Warn,
                    Kind::Ftms,
                    "Control Point write with no op code",
                );
                return reject(op::REQUEST_CONTROL, result::INVALID_PARAMETER);
            }
            Err(ParseError::InvalidParameter(opcode)) => {
                let cmd = descriptor(opcode);
                let summary = format!("{} (invalid parameter: malformed payload)", cmd.summary);
                self.finish(
                    client,
                    now_ms,
                    &cmd,
                    result::INVALID_PARAMETER,
                    summary,
                    None,
                    raw,
                    log,
                );
                return reject(opcode, result::INVALID_PARAMETER);
            }
        };

        let cmd = describe(&request);
        let opcode = cmd.opcode;

        // Permission gate: everything except Request Control needs ownership.
        if request != ControlRequest::RequestControl && self.owner != Some(client) {
            let summary = format!("{} (rejected: no control permission)", cmd.summary);
            self.finish(
                client,
                now_ms,
                &cmd,
                result::CONTROL_NOT_PERMITTED,
                summary,
                cmd.value,
                raw,
                log,
            );
            return reject(opcode, result::CONTROL_NOT_PERMITTED);
        }

        let mut status_msg: Option<Vec<u8>> = None;
        let mut detail: Vec<String> = Vec::new();
        let res = match request {
            ControlRequest::RequestControl => match self.owner {
                None => {
                    self.owner = Some(client);
                    result::SUCCESS
                }
                Some(o) if o == client => result::SUCCESS,
                Some(_) => result::CONTROL_NOT_PERMITTED,
            },
            ControlRequest::Reset => {
                self.clear_targets();
                self.simulation = None;
                self.machine = MachineState::Stopped;
                status_msg = Some(ftms::machine_status(status::RESET, &[]));
                result::SUCCESS
            }
            ControlRequest::SetTargetResistance(raw_level) => {
                if raw_level > ftms::RESISTANCE_RAW_MAX {
                    result::INVALID_PARAMETER
                } else {
                    self.target_resistance = Some(raw_level);
                    self.target_power = None;
                    status_msg = Some(ftms::machine_status(
                        status::TARGET_RESISTANCE_CHANGED,
                        &[raw_level],
                    ));
                    result::SUCCESS
                }
            }
            ControlRequest::SetTargetPower(w) => {
                if !(ftms::POWER_MIN_W..=ftms::POWER_MAX_W).contains(&w) {
                    result::INVALID_PARAMETER
                } else {
                    self.target_power = Some(w);
                    self.target_resistance = None;
                    status_msg = Some(ftms::machine_status(
                        status::TARGET_POWER_CHANGED,
                        &w.to_le_bytes(),
                    ));
                    result::SUCCESS
                }
            }
            ControlRequest::StartOrResume => {
                self.machine = MachineState::Started;
                status_msg = Some(ftms::machine_status(status::STARTED_OR_RESUMED, &[]));
                result::SUCCESS
            }
            ControlRequest::StopOrPause(p @ (1 | 2)) => {
                if p == 1 {
                    self.machine = MachineState::Stopped;
                    self.clear_targets();
                } else {
                    self.machine = MachineState::Paused;
                }
                status_msg = Some(ftms::machine_status(status::STOPPED_OR_PAUSED, &[p]));
                result::SUCCESS
            }
            ControlRequest::StopOrPause(_) => result::INVALID_PARAMETER,
            ControlRequest::SetIndoorBikeSimulation(sim) => {
                self.simulation = Some(sim);
                // Simulation mode supersedes ERG / resistance targets.
                self.clear_targets();
                status_msg = Some(ftms::machine_status(
                    status::INDOOR_BIKE_SIMULATION_CHANGED,
                    &sim.to_bytes(),
                ));
                detail = alloc::vec![
                    format!("Wind speed: {} m/s", sim.wind_speed_ms()),
                    format!("Grade: {:.1} %", sim.grade_percent()),
                    format!("Crr: {:.4}", sim.crr()),
                    format!("Cw: {:.2}", sim.cw()),
                ];
                result::SUCCESS
            }
            // Op codes the simulated trainer does not advertise in its Target Setting Features.
            ControlRequest::SetTargetSpeed(_)
            | ControlRequest::SetTargetInclination(_)
            | ControlRequest::SetTargetHeartRate(_)
            | ControlRequest::SetWheelCircumference(_)
            | ControlRequest::SpinDown(_)
            | ControlRequest::SetTargetCadence(_)
            | ControlRequest::Other(_) => result::NOT_SUPPORTED,
        };

        let accepted = res == result::SUCCESS;
        let summary = if accepted {
            cmd.summary.clone()
        } else {
            format!("{} ({})", cmd.summary, result_text(res))
        };
        self.finish(client, now_ms, &cmd, res, summary, cmd.value, raw, log);
        if !detail.is_empty() && accepted {
            // Attach the parameter breakdown to the event we just recorded.
            if let Some(e) = log.last_mut() {
                e.detail = detail;
            }
        }
        if accepted && request == ControlRequest::RequestControl {
            log.record(
                now_ms,
                Level::Info,
                Kind::Ftms,
                format!("Control granted to client {client}"),
            );
        }
        ControlOutcome {
            response: ftms::control_response(opcode, res),
            status: if accepted { status_msg } else { None },
            accepted,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        client: ClientId,
        now_ms: u64,
        cmd: &Descriptor,
        res: u8,
        summary: String,
        value: Option<f32>,
        raw: String,
        log: &mut EventLog,
    ) {
        let result_name = result_name(res);
        self.last_command = Some(LastCommand {
            t: now_ms,
            command: cmd.command,
            name: cmd.name,
            result: result_name,
            client,
            summary: summary.clone(),
        });
        let level = if res == result::SUCCESS {
            Level::Info
        } else {
            Level::Warn
        };
        log.record_full(
            now_ms,
            level,
            Kind::Ftms,
            summary,
            Vec::new(),
            Some(FtmsCommand {
                command: cmd.command,
                name: cmd.name,
                value,
                unit: cmd.unit,
                result: result_name,
                client,
                raw,
            }),
        );
    }
}

fn reject(opcode: u8, res: u8) -> ControlOutcome {
    ControlOutcome {
        response: ftms::control_response(opcode, res),
        status: None,
        accepted: false,
    }
}

fn hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn result_name(res: u8) -> &'static str {
    match res {
        result::SUCCESS => "success",
        result::NOT_SUPPORTED => "notSupported",
        result::INVALID_PARAMETER => "invalidParameter",
        result::CONTROL_NOT_PERMITTED => "controlNotPermitted",
        _ => "operationFailed",
    }
}

fn result_text(res: u8) -> &'static str {
    match res {
        result::SUCCESS => "ok",
        result::NOT_SUPPORTED => "not supported",
        result::INVALID_PARAMETER => "invalid parameter",
        result::CONTROL_NOT_PERMITTED => "control not permitted",
        _ => "operation failed",
    }
}

struct Descriptor {
    opcode: u8,
    command: &'static str,
    name: &'static str,
    unit: Option<&'static str>,
    value: Option<f32>,
    summary: String,
}

fn descriptor(opcode: u8) -> Descriptor {
    let name = ftms::op_name(opcode);
    let command = match opcode {
        op::REQUEST_CONTROL => "requestControl",
        op::RESET => "reset",
        op::SET_TARGET_SPEED => "setTargetSpeed",
        op::SET_TARGET_INCLINATION => "setTargetInclination",
        op::SET_TARGET_RESISTANCE => "setTargetResistance",
        op::SET_TARGET_POWER => "setTargetPower",
        op::SET_TARGET_HEART_RATE => "setTargetHeartRate",
        op::START_OR_RESUME => "start",
        op::STOP_OR_PAUSE => "stopOrPause",
        op::SET_INDOOR_BIKE_SIMULATION => "setIndoorBikeSimulation",
        op::SET_WHEEL_CIRCUMFERENCE => "setWheelCircumference",
        op::SPIN_DOWN => "spinDown",
        op::SET_TARGET_CADENCE => "setTargetCadence",
        _ => "unknown",
    };
    Descriptor {
        opcode,
        command,
        name,
        unit: None,
        value: None,
        summary: name.into(),
    }
}

fn describe(req: &ControlRequest) -> Descriptor {
    match *req {
        ControlRequest::RequestControl => descriptor(op::REQUEST_CONTROL),
        ControlRequest::Reset => descriptor(op::RESET),
        ControlRequest::StartOrResume => descriptor(op::START_OR_RESUME),
        ControlRequest::SetTargetPower(w) => Descriptor {
            unit: Some("W"),
            value: Some(w as f32),
            summary: format!("Set Target Power = {w} W"),
            ..descriptor(op::SET_TARGET_POWER)
        },
        ControlRequest::SetTargetResistance(raw) => Descriptor {
            unit: Some("%"),
            value: Some(raw as f32),
            summary: format!(
                "Set Target Resistance Level = {:.1} ({} %)",
                raw as f32 / 10.0,
                raw
            ),
            ..descriptor(op::SET_TARGET_RESISTANCE)
        },
        ControlRequest::StopOrPause(p) => {
            let mut d = descriptor(op::STOP_OR_PAUSE);
            match p {
                1 => {
                    d.command = "stop";
                    d.summary = "Stop".into();
                }
                2 => {
                    d.command = "pause";
                    d.summary = "Pause".into();
                }
                other => d.summary = format!("Stop / Pause (invalid parameter {other})"),
            }
            d
        }
        ControlRequest::SetIndoorBikeSimulation(_) => descriptor(op::SET_INDOOR_BIKE_SIMULATION),
        ControlRequest::SetTargetSpeed(v) => Descriptor {
            unit: Some("km/h"),
            value: Some(v as f32 * 0.01),
            summary: format!("Set Target Speed = {:.2} km/h", v as f32 * 0.01),
            ..descriptor(op::SET_TARGET_SPEED)
        },
        ControlRequest::SetTargetInclination(v) => Descriptor {
            unit: Some("%"),
            value: Some(v as f32 * 0.1),
            summary: format!("Set Target Inclination = {:.1} %", v as f32 * 0.1),
            ..descriptor(op::SET_TARGET_INCLINATION)
        },
        ControlRequest::SetTargetHeartRate(v) => Descriptor {
            unit: Some("bpm"),
            value: Some(v as f32),
            summary: format!("Set Target Heart Rate = {v} bpm"),
            ..descriptor(op::SET_TARGET_HEART_RATE)
        },
        ControlRequest::SetWheelCircumference(v) => Descriptor {
            unit: Some("mm"),
            value: Some(v as f32 * 0.1),
            summary: format!("Set Wheel Circumference = {:.1} mm", v as f32 * 0.1),
            ..descriptor(op::SET_WHEEL_CIRCUMFERENCE)
        },
        ControlRequest::SpinDown(p) => Descriptor {
            summary: format!(
                "Spin Down Control ({})",
                if p == 1 { "start" } else { "ignore" }
            ),
            ..descriptor(op::SPIN_DOWN)
        },
        ControlRequest::SetTargetCadence(v) => Descriptor {
            unit: Some("rpm"),
            value: Some(v as f32 * 0.5),
            summary: format!("Set Targeted Cadence = {:.1} rpm", v as f32 * 0.5),
            ..descriptor(op::SET_TARGET_CADENCE)
        },
        ControlRequest::Other(opcode) => {
            let mut d = descriptor(opcode);
            if d.name == "Unknown op code" {
                d.summary = format!("Unknown op code 0x{opcode:02x}");
            }
            d
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: ClientId = 64;

    fn write(
        t: &mut TrainerState,
        log: &mut EventLog,
        client: ClientId,
        data: &[u8],
    ) -> ControlOutcome {
        t.handle_write(client, data, log, 1000)
    }

    fn granted() -> (TrainerState, EventLog) {
        let mut t = TrainerState::default();
        let mut log = EventLog::default();
        assert!(write(&mut t, &mut log, ME, &[0x00]).accepted);
        (t, log)
    }

    #[test]
    fn zwift_style_session() {
        let (mut t, mut log) = granted();
        assert_eq!(t.owner, Some(ME));
        let o = write(&mut t, &mut log, ME, &[0x07]);
        assert_eq!(o.response, [0x80, 0x07, 0x01]);
        assert_eq!(o.status.as_deref(), Some(&[0x04][..]));
        assert_eq!(t.machine, MachineState::Started);

        let o = write(&mut t, &mut log, ME, &[0x05, 200, 0]);
        assert_eq!(o.response, [0x80, 0x05, 0x01]);
        assert_eq!(o.status.as_deref(), Some(&[0x08, 200, 0][..]));
        assert_eq!(t.target_power, Some(200));
        assert_eq!(t.training_status(), ftms::TrainingStatus::WattControl);

        // grade 4 %, crr 0.004, cw 0.50
        let o = write(&mut t, &mut log, ME, &[0x11, 0, 0, 0x90, 0x01, 40, 50]);
        assert!(o.accepted);
        assert_eq!(t.target_power, None, "simulation supersedes ERG");
        assert_eq!(t.road().unwrap().grade, 4.0);
        let last = log.iter().last().unwrap();
        assert_eq!(last.message, "Set Indoor Bike Simulation Parameters");
        assert_eq!(
            last.detail,
            [
                "Wind speed: 0 m/s",
                "Grade: 4.0 %",
                "Crr: 0.0040",
                "Cw: 0.50"
            ]
        );
        assert_eq!(
            t.last_command.as_ref().unwrap().command,
            "setIndoorBikeSimulation"
        );
    }

    #[test]
    fn logs_match_the_spec_example() {
        let (mut t, mut log) = granted();
        write(&mut t, &mut log, ME, &[0x05, 180, 0]);
        let msgs: Vec<&str> = log.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            msgs,
            [
                "Request Control",
                "Control granted to client 64",
                "Set Target Power = 180 W"
            ]
        );
        let ftms = log.iter().last().unwrap().ftms.as_ref().unwrap();
        assert_eq!(ftms.command, "setTargetPower");
        assert_eq!(ftms.value, Some(180.0));
        assert_eq!(ftms.unit, Some("W"));
        assert_eq!(ftms.client, ME);
        assert_eq!(ftms.raw, "05b400");
    }

    #[test]
    fn commands_need_control() {
        let mut t = TrainerState::default();
        let mut log = EventLog::default();
        let o = write(&mut t, &mut log, ME, &[0x05, 200, 0]);
        assert_eq!(o.response, [0x80, 0x05, 0x05]);
        assert!(!o.accepted && o.status.is_none());
        assert_eq!(t.target_power, None);
    }

    #[test]
    fn second_client_cannot_steal_control() {
        let (mut t, mut log) = granted();
        let o = write(&mut t, &mut log, 99, &[0x00]);
        assert_eq!(o.response, [0x80, 0x00, 0x05]);
        assert_eq!(t.owner, Some(ME));
        // idempotent for the owner
        assert!(write(&mut t, &mut log, ME, &[0x00]).accepted);
        // after the owner leaves, the other client can take over
        t.on_disconnect(ME, &mut log, 5);
        assert_eq!(t.owner, None);
        assert!(write(&mut t, &mut log, 99, &[0x00]).accepted);
        assert_eq!(t.owner, Some(99));
    }

    #[test]
    fn parameter_validation() {
        let (mut t, mut log) = granted();
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x05, 0xD1, 0x07]).response,
            [0x80, 0x05, 0x03]
        ); // 2001 W
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x05, 0xFF, 0xFF]).response,
            [0x80, 0x05, 0x03]
        ); // -1 W
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x04, 101]).response,
            [0x80, 0x04, 0x03]
        );
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x08, 7]).response,
            [0x80, 0x08, 0x03]
        );
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x05, 1]).response,
            [0x80, 0x05, 0x03]
        ); // truncated
        assert_eq!(t.target_power, None);
    }

    #[test]
    fn unsupported_ops_are_rejected_politely() {
        let (mut t, mut log) = granted();
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x02, 0x10, 0x27]).response,
            [0x80, 0x02, 0x02]
        );
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x13, 0x01]).response,
            [0x80, 0x13, 0x02]
        );
        assert_eq!(
            write(&mut t, &mut log, ME, &[0x42]).response,
            [0x80, 0x42, 0x02]
        );
        assert_eq!(
            log.iter().last().unwrap().message,
            "Unknown op code 0x42 (not supported)"
        );
        assert_eq!(
            log.iter().last().unwrap().ftms.as_ref().unwrap().result,
            "notSupported"
        );
    }

    #[test]
    fn resistance_and_power_targets_are_exclusive() {
        let (mut t, mut log) = granted();
        write(&mut t, &mut log, ME, &[0x05, 150, 0]);
        let o = write(&mut t, &mut log, ME, &[0x04, 42]);
        assert_eq!(o.status.as_deref(), Some(&[0x07, 42][..]));
        assert_eq!((t.target_power, t.target_resistance), (None, Some(42)));
        write(&mut t, &mut log, ME, &[0x05, 150, 0]);
        assert_eq!((t.target_power, t.target_resistance), (Some(150), None));
    }

    #[test]
    fn stop_pause_reset() {
        let (mut t, mut log) = granted();
        write(&mut t, &mut log, ME, &[0x07]);
        write(&mut t, &mut log, ME, &[0x05, 150, 0]);
        let o = write(&mut t, &mut log, ME, &[0x08, 2]);
        assert!(o.accepted);
        assert_eq!(t.machine, MachineState::Paused);
        assert_eq!(t.target_power, Some(150), "pause keeps the target");
        let o = write(&mut t, &mut log, ME, &[0x08, 1]);
        assert_eq!(t.machine, MachineState::Stopped);
        assert_eq!(o.status.as_deref(), Some(&[0x02, 1][..]));
        assert_eq!(t.target_power, None);
        write(&mut t, &mut log, ME, &[0x11, 0, 0, 0, 0, 40, 51]);
        let o = write(&mut t, &mut log, ME, &[0x01]);
        assert!(o.accepted);
        assert!(t.simulation.is_none());
        assert_eq!(t.owner, Some(ME), "control is retained across reset");
        assert_eq!(log.iter().filter(|e| e.message == "Stop").count(), 1);
    }

    #[test]
    fn empty_write_is_handled() {
        let mut t = TrainerState::default();
        let mut log = EventLog::default();
        let o = t.handle_write(ME, &[], &mut log, 0);
        assert!(!o.accepted);
    }

    #[test]
    fn counts_commands() {
        let (mut t, mut log) = granted();
        write(&mut t, &mut log, ME, &[0x07]);
        assert_eq!(t.commands_received, 2);
    }
}
