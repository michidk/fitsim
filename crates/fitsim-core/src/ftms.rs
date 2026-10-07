//! Bluetooth SIG **Fitness Machine Service** (FTMS 1.0) wire formats for an indoor bike.
//!
//! Only byte-level encode/decode lives here; behaviour (control ownership, targets, …) is in
//! [`crate::trainer`].

use alloc::vec::Vec;

/// Fitness Machine Service (Bluetooth SIG assigned number). Characteristic UUIDs live in the
/// `#[gatt_service]` definition in the firmware.
pub const SERVICE_UUID: u16 = 0x1826;

// ---- Supported ranges (advertised through the range characteristics) --------------------------
/// Resistance level in 0.1 steps: raw `0..=100` maps 1:1 onto the simulator's 0–100 % resistance.
pub const RESISTANCE_RAW_MAX: u8 = 100;
pub const POWER_MIN_W: i16 = 0;
pub const POWER_MAX_W: i16 = 2000;

/// Fitness Machine Feature: bits 1 cadence, 7 resistance level, 10 heart rate, 14 power measurement.
pub const MACHINE_FEATURES: u32 = (1 << 1) | (1 << 7) | (1 << 10) | (1 << 14);
/// Target Setting Features: bit 2 resistance target, bit 3 power target, bit 13 indoor bike simulation.
pub const TARGET_FEATURES: u32 = (1 << 2) | (1 << 3) | (1 << 13);

/// `Fitness Machine Feature` characteristic value (two little-endian `uint32`).
pub fn feature_bytes() -> [u8; 8] {
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&MACHINE_FEATURES.to_le_bytes());
    b[4..].copy_from_slice(&TARGET_FEATURES.to_le_bytes());
    b
}

/// `Supported Resistance Level Range`: min `sint16`, max `sint16`, increment `uint16` (0.1 unitless).
pub fn supported_resistance_range() -> [u8; 6] {
    let mut b = [0u8; 6];
    b[..2].copy_from_slice(&0i16.to_le_bytes());
    b[2..4].copy_from_slice(&(RESISTANCE_RAW_MAX as i16).to_le_bytes());
    b[4..].copy_from_slice(&1u16.to_le_bytes());
    b
}

/// `Supported Power Range`: min `sint16` W, max `sint16` W, increment `uint16` W.
pub fn supported_power_range() -> [u8; 6] {
    let mut b = [0u8; 6];
    b[..2].copy_from_slice(&POWER_MIN_W.to_le_bytes());
    b[2..4].copy_from_slice(&POWER_MAX_W.to_le_bytes());
    b[4..].copy_from_slice(&1u16.to_le_bytes());
    b
}

/// Service Data AD structure payload required by FTMS: availability flag + machine type (indoor bike).
pub fn service_data() -> [u8; 3] {
    [0x01, 0x20, 0x00]
}

// ---- Indoor Bike Data -------------------------------------------------------------------------
mod flag {
    // Bit 0 is "More Data": *cleared* means Instantaneous Speed is present.
    pub const CADENCE: u16 = 1 << 2;
    pub const RESISTANCE: u16 = 1 << 5;
    pub const POWER: u16 = 1 << 6;
    pub const HEART_RATE: u16 = 1 << 9;
}

pub const BIKE_DATA_MAX_LEN: usize = 11;

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BikeSample {
    pub speed_kmh: f32,
    pub cadence_rpm: f32,
    /// 0–100 (unitless resistance level as reported by the trainer).
    pub resistance: f32,
    pub power_w: f32,
    pub heart_rate: f32,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Packet<const N: usize> {
    pub bytes: [u8; N],
    pub len: usize,
}

impl<const N: usize> Packet<N> {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

fn sat_u16(x: f32) -> u16 {
    crate::math::round(x.max(0.0)) as u16
}
fn sat_i16(x: f32) -> i16 {
    crate::math::round(x) as i16
}

/// Encodes an *Indoor Bike Data* notification (speed, cadence, resistance, power, heart rate),
/// dropping optional fields (heart rate first, then resistance) until it fits in `max_len` bytes.
/// `max_len` is `ATT_MTU - 3`; even the default MTU of 23 leaves room for everything (11 bytes).
pub fn encode_indoor_bike_data(s: &BikeSample, max_len: usize) -> Packet<BIKE_DATA_MAX_LEN> {
    // Mandatory core: flags(2) + speed(2) + cadence(2) + power(2).
    let mut len = 8;
    let mut flags = flag::CADENCE | flag::POWER;
    let optional: [(u16, usize); 2] = [(flag::RESISTANCE, 2), (flag::HEART_RATE, 1)];
    for (bit, size) in optional {
        if len + size <= max_len {
            flags |= bit;
            len += size;
        }
    }

    let mut out = [0u8; BIKE_DATA_MAX_LEN];
    let mut n = 0;
    let mut put = |bytes: &[u8]| {
        out[n..n + bytes.len()].copy_from_slice(bytes);
        n += bytes.len();
    };
    put(&flags.to_le_bytes());
    put(&sat_u16(s.speed_kmh * 100.0).to_le_bytes()); // 0.01 km/h
    put(&sat_u16(s.cadence_rpm * 2.0).to_le_bytes()); // 0.5 rpm
    if flags & flag::RESISTANCE != 0 {
        put(&sat_i16(s.resistance).to_le_bytes());
    }
    put(&sat_i16(s.power_w).to_le_bytes());
    if flags & flag::HEART_RATE != 0 {
        put(&[s.heart_rate.clamp(0.0, 255.0) as u8]);
    }
    debug_assert_eq!(n, len);
    Packet { bytes: out, len: n }
}

// ---- Training Status --------------------------------------------------------------------------
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TrainingStatus {
    Other = 0x00,
    Idle = 0x01,
    WarmingUp = 0x02,
    WattControl = 0x0C,
    ManualMode = 0x0D,
}

pub fn training_status(status: TrainingStatus) -> [u8; 2] {
    [0x00, status as u8] // flags: no string / extended string
}

// ---- Control Point ----------------------------------------------------------------------------
pub mod op {
    pub const REQUEST_CONTROL: u8 = 0x00;
    pub const RESET: u8 = 0x01;
    pub const SET_TARGET_SPEED: u8 = 0x02;
    pub const SET_TARGET_INCLINATION: u8 = 0x03;
    pub const SET_TARGET_RESISTANCE: u8 = 0x04;
    pub const SET_TARGET_POWER: u8 = 0x05;
    pub const SET_TARGET_HEART_RATE: u8 = 0x06;
    pub const START_OR_RESUME: u8 = 0x07;
    pub const STOP_OR_PAUSE: u8 = 0x08;
    pub const SET_INDOOR_BIKE_SIMULATION: u8 = 0x11;
    pub const SET_WHEEL_CIRCUMFERENCE: u8 = 0x12;
    pub const SPIN_DOWN: u8 = 0x13;
    pub const SET_TARGET_CADENCE: u8 = 0x14;
    pub const RESPONSE_CODE: u8 = 0x80;
}

pub mod result {
    pub const SUCCESS: u8 = 0x01;
    pub const NOT_SUPPORTED: u8 = 0x02;
    pub const INVALID_PARAMETER: u8 = 0x03;
    pub const OPERATION_FAILED: u8 = 0x04;
    pub const CONTROL_NOT_PERMITTED: u8 = 0x05;
}

/// A decoded Control Point request.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ControlRequest {
    RequestControl,
    Reset,
    /// Unsigned, 0.01 km/h.
    SetTargetSpeed(u16),
    /// Signed, 0.1 %.
    SetTargetInclination(i16),
    /// Unsigned, 0.1 unitless.
    SetTargetResistance(u8),
    /// Signed, 1 W.
    SetTargetPower(i16),
    SetTargetHeartRate(u8),
    StartOrResume,
    /// 0x01 = stop, 0x02 = pause.
    StopOrPause(u8),
    SetIndoorBikeSimulation(IndoorBikeSimulation),
    SetWheelCircumference(u16),
    SpinDown(u8),
    SetTargetCadence(u16),
    /// Any other op code (still answered with `Op Code Not Supported`).
    Other(u8),
}

/// Raw "Set Indoor Bike Simulation Parameters" payload.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct IndoorBikeSimulation {
    /// 0.001 m/s
    pub wind_speed_raw: i16,
    /// 0.01 %
    pub grade_raw: i16,
    /// 0.0001
    pub crr_raw: u8,
    /// 0.01 kg/m
    pub cw_raw: u8,
}

impl IndoorBikeSimulation {
    pub fn wind_speed_ms(&self) -> f32 {
        self.wind_speed_raw as f32 * 0.001
    }
    pub fn grade_percent(&self) -> f32 {
        self.grade_raw as f32 * 0.01
    }
    pub fn crr(&self) -> f32 {
        self.crr_raw as f32 * 0.0001
    }
    pub fn cw(&self) -> f32 {
        self.cw_raw as f32 * 0.01
    }
    pub fn to_bytes(&self) -> [u8; 6] {
        let mut b = [0u8; 6];
        b[..2].copy_from_slice(&self.wind_speed_raw.to_le_bytes());
        b[2..4].copy_from_slice(&self.grade_raw.to_le_bytes());
        b[4] = self.crr_raw;
        b[5] = self.cw_raw;
        b
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Zero-length write.
    Empty,
    /// Known op code with a payload of the wrong size.
    InvalidParameter(u8),
}

/// Decodes a Control Point write. Op codes the simulator does not implement still decode (as
/// [`ControlRequest::Other`] or their specific variant) so they can be logged and rejected with
/// `Op Code Not Supported`.
pub fn parse_control_point(data: &[u8]) -> Result<ControlRequest, ParseError> {
    let (&opcode, params) = data.split_first().ok_or(ParseError::Empty)?;
    let bad = ParseError::InvalidParameter(opcode);
    let exact = |n: usize| if params.len() == n { Ok(()) } else { Err(bad) };
    Ok(match opcode {
        op::REQUEST_CONTROL => {
            exact(0)?;
            ControlRequest::RequestControl
        }
        op::RESET => {
            exact(0)?;
            ControlRequest::Reset
        }
        op::SET_TARGET_SPEED => {
            exact(2)?;
            ControlRequest::SetTargetSpeed(u16::from_le_bytes([params[0], params[1]]))
        }
        op::SET_TARGET_INCLINATION => {
            exact(2)?;
            ControlRequest::SetTargetInclination(i16::from_le_bytes([params[0], params[1]]))
        }
        op::SET_TARGET_RESISTANCE => {
            exact(1)?;
            ControlRequest::SetTargetResistance(params[0])
        }
        op::SET_TARGET_POWER => {
            exact(2)?;
            ControlRequest::SetTargetPower(i16::from_le_bytes([params[0], params[1]]))
        }
        op::SET_TARGET_HEART_RATE => {
            exact(1)?;
            ControlRequest::SetTargetHeartRate(params[0])
        }
        op::START_OR_RESUME => {
            exact(0)?;
            ControlRequest::StartOrResume
        }
        op::STOP_OR_PAUSE => {
            exact(1)?;
            ControlRequest::StopOrPause(params[0])
        }
        op::SET_INDOOR_BIKE_SIMULATION => {
            exact(6)?;
            ControlRequest::SetIndoorBikeSimulation(IndoorBikeSimulation {
                wind_speed_raw: i16::from_le_bytes([params[0], params[1]]),
                grade_raw: i16::from_le_bytes([params[2], params[3]]),
                crr_raw: params[4],
                cw_raw: params[5],
            })
        }
        op::SET_WHEEL_CIRCUMFERENCE => {
            exact(2)?;
            ControlRequest::SetWheelCircumference(u16::from_le_bytes([params[0], params[1]]))
        }
        op::SPIN_DOWN => {
            exact(1)?;
            ControlRequest::SpinDown(params[0])
        }
        op::SET_TARGET_CADENCE => {
            exact(2)?;
            ControlRequest::SetTargetCadence(u16::from_le_bytes([params[0], params[1]]))
        }
        other => ControlRequest::Other(other),
    })
}

/// Control Point indication: `[0x80, request op code, result]`.
pub fn control_response(request_op: u8, result: u8) -> [u8; 3] {
    [op::RESPONSE_CODE, request_op, result]
}

pub fn op_name(opcode: u8) -> &'static str {
    match opcode {
        op::REQUEST_CONTROL => "Request Control",
        op::RESET => "Reset",
        op::SET_TARGET_SPEED => "Set Target Speed",
        op::SET_TARGET_INCLINATION => "Set Target Inclination",
        op::SET_TARGET_RESISTANCE => "Set Target Resistance Level",
        op::SET_TARGET_POWER => "Set Target Power",
        op::SET_TARGET_HEART_RATE => "Set Target Heart Rate",
        op::START_OR_RESUME => "Start / Resume",
        op::STOP_OR_PAUSE => "Stop / Pause",
        0x09 => "Set Targeted Expended Energy",
        0x0A => "Set Targeted Number of Steps",
        0x0B => "Set Targeted Number of Strides",
        0x0C => "Set Targeted Distance",
        0x0D => "Set Targeted Training Time",
        0x0E => "Set Targeted Time in Two Heart Rate Zones",
        0x0F => "Set Targeted Time in Three Heart Rate Zones",
        0x10 => "Set Targeted Time in Five Heart Rate Zones",
        op::SET_INDOOR_BIKE_SIMULATION => "Set Indoor Bike Simulation Parameters",
        op::SET_WHEEL_CIRCUMFERENCE => "Set Wheel Circumference",
        op::SPIN_DOWN => "Spin Down Control",
        op::SET_TARGET_CADENCE => "Set Targeted Cadence",
        _ => "Unknown op code",
    }
}

// ---- Fitness Machine Status -------------------------------------------------------------------
pub mod status {
    pub const RESET: u8 = 0x01;
    pub const STOPPED_OR_PAUSED: u8 = 0x02;
    pub const STARTED_OR_RESUMED: u8 = 0x04;
    pub const TARGET_RESISTANCE_CHANGED: u8 = 0x07;
    pub const TARGET_POWER_CHANGED: u8 = 0x08;
    pub const INDOOR_BIKE_SIMULATION_CHANGED: u8 = 0x12;
    pub const CONTROL_PERMISSION_LOST: u8 = 0xFF;
}

/// Builds a Fitness Machine Status notification.
pub fn machine_status(code: u8, params: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + params.len());
    v.push(code);
    v.extend_from_slice(params);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BikeSample {
        BikeSample {
            speed_kmh: 32.4,
            cadence_rpm: 92.0,
            resistance: 42.0,
            power_w: 245.0,
            heart_rate: 154.0,
        }
    }

    #[test]
    fn full_bike_data_layout() {
        let p = encode_indoor_bike_data(&sample(), 244);
        assert_eq!(p.len, 11);
        let b = p.as_slice();
        // flags: cadence | resistance | power | heart rate
        let flags = u16::from_le_bytes([b[0], b[1]]);
        assert_eq!(flags, (1 << 2) | (1 << 5) | (1 << 6) | (1 << 9));
        assert_eq!(
            flags & 1,
            0,
            "bit 0 must be clear so instantaneous speed is present"
        );
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), 3240); // 32.4 km/h
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 184); // 92 rpm in 0.5 steps
        assert_eq!(i16::from_le_bytes([b[6], b[7]]), 42); // resistance
        assert_eq!(i16::from_le_bytes([b[8], b[9]]), 245); // power
        assert_eq!(b[10], 154); // heart rate
    }

    #[test]
    fn everything_fits_the_default_mtu() {
        // Default ATT_MTU 23 leaves 20 payload bytes.
        assert_eq!(encode_indoor_bike_data(&sample(), 20).len, 11);
    }

    #[test]
    fn bike_data_drops_optional_fields_for_tiny_mtus() {
        let p = encode_indoor_bike_data(&sample(), 10);
        assert_eq!(p.len, 10, "heart rate goes first");
        assert_eq!(
            u16::from_le_bytes([p.bytes[0], p.bytes[1]]),
            (1 << 2) | (1 << 5) | (1 << 6)
        );
        let p = encode_indoor_bike_data(&sample(), 8);
        assert_eq!(p.len, 8, "only the mandatory core remains");
        assert_eq!(
            u16::from_le_bytes([p.bytes[0], p.bytes[1]]),
            (1 << 2) | (1 << 6)
        );
        assert_eq!(i16::from_le_bytes([p.bytes[6], p.bytes[7]]), 245);
    }

    #[test]
    fn bike_data_saturates_extremes() {
        let s = BikeSample {
            speed_kmh: 100_000.0,
            cadence_rpm: -3.0,
            power_w: 99_999.0,
            ..sample()
        };
        let p = encode_indoor_bike_data(&s, 244);
        assert_eq!(u16::from_le_bytes([p.bytes[2], p.bytes[3]]), u16::MAX);
        assert_eq!(u16::from_le_bytes([p.bytes[4], p.bytes[5]]), 0);
        assert_eq!(i16::from_le_bytes([p.bytes[8], p.bytes[9]]), i16::MAX);
    }

    #[test]
    fn static_characteristics() {
        let f = feature_bytes();
        assert_eq!(
            u32::from_le_bytes(f[..4].try_into().unwrap()),
            MACHINE_FEATURES
        );
        assert_eq!(
            u32::from_le_bytes(f[4..].try_into().unwrap()),
            TARGET_FEATURES
        );
        assert_ne!(MACHINE_FEATURES & (1 << 14), 0, "power measurement");
        assert_ne!(TARGET_FEATURES & (1 << 3), 0, "power target");
        assert_ne!(TARGET_FEATURES & (1 << 13), 0, "indoor bike simulation");
        assert_eq!(supported_power_range(), [0, 0, 0xD0, 0x07, 1, 0]);
        assert_eq!(supported_resistance_range(), [0, 0, 100, 0, 1, 0]);
        assert_eq!(service_data(), [1, 0x20, 0]);
    }

    #[test]
    fn parses_zwift_style_command_sequence() {
        assert_eq!(
            parse_control_point(&[0x00]),
            Ok(ControlRequest::RequestControl)
        );
        assert_eq!(
            parse_control_point(&[0x07]),
            Ok(ControlRequest::StartOrResume)
        );
        assert_eq!(
            parse_control_point(&[0x05, 0xC8, 0x00]),
            Ok(ControlRequest::SetTargetPower(200))
        );
        assert_eq!(
            parse_control_point(&[0x04, 42]),
            Ok(ControlRequest::SetTargetResistance(42))
        );
        assert_eq!(
            parse_control_point(&[0x08, 0x01]),
            Ok(ControlRequest::StopOrPause(1))
        );
        // grade 4.00 %, crr 0.0040, cw 0.50, wind 0
        let req = parse_control_point(&[0x11, 0x00, 0x00, 0x90, 0x01, 40, 50]).unwrap();
        let ControlRequest::SetIndoorBikeSimulation(sim) = req else {
            panic!()
        };
        assert_eq!(sim.grade_percent(), 4.0);
        assert!((sim.crr() - 0.004).abs() < 1e-6);
        assert!((sim.cw() - 0.5).abs() < 1e-6);
        assert_eq!(sim.wind_speed_ms(), 0.0);
        assert_eq!(sim.to_bytes(), [0x00, 0x00, 0x90, 0x01, 40, 50]);
    }

    #[test]
    fn negative_values_decode_signed() {
        let ControlRequest::SetIndoorBikeSimulation(sim) =
            parse_control_point(&[0x11, 0x18, 0xFB, 0xF4, 0xFD, 40, 51]).unwrap()
        else {
            panic!()
        };
        assert_eq!(sim.wind_speed_raw, -1256);
        assert!(
            (sim.grade_percent() + 5.24).abs() < 1e-3,
            "{}",
            sim.grade_percent()
        );
    }

    #[test]
    fn rejects_malformed_writes() {
        assert_eq!(parse_control_point(&[]), Err(ParseError::Empty));
        assert_eq!(
            parse_control_point(&[0x05, 0x01]),
            Err(ParseError::InvalidParameter(0x05))
        );
        assert_eq!(
            parse_control_point(&[0x00, 0x00]),
            Err(ParseError::InvalidParameter(0x00))
        );
        assert_eq!(
            parse_control_point(&[0x11, 1, 2, 3]),
            Err(ParseError::InvalidParameter(0x11))
        );
        assert_eq!(
            parse_control_point(&[0x42]),
            Ok(ControlRequest::Other(0x42))
        );
    }

    #[test]
    fn response_and_status_framing() {
        assert_eq!(
            control_response(op::SET_TARGET_POWER, result::SUCCESS),
            [0x80, 0x05, 0x01]
        );
        assert_eq!(
            machine_status(status::TARGET_POWER_CHANGED, &250i16.to_le_bytes()),
            [0x08, 0xFA, 0x00]
        );
        assert_eq!(training_status(TrainingStatus::WattControl), [0x00, 0x0C]);
    }
}
