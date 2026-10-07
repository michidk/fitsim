//! Bluetooth SIG **Cycling Power Service** (CPS 1.1).

pub const SERVICE_UUID: u16 = 0x1818;

pub const MEASUREMENT_LEN: usize = 8;

/// Cycling Power Feature: bit 3 crank revolution data.
pub const FEATURES: u32 = 1 << 3;
/// Sensor Location 5 = left crank.
pub const SENSOR_LOCATION_LEFT_CRANK: u8 = 5;

pub fn feature_bytes() -> [u8; 4] {
    FEATURES.to_le_bytes()
}

/// Measurement flags: bit 5 crank revolution data present.
const FLAGS: u16 = 1 << 5;

/// Encodes a Cycling Power Measurement:
/// `flags(2) | instantaneous power sint16 (W) | cumulative crank revs u16 | last crank event time
/// u16 (1/1024 s)`.
pub fn encode_measurement(
    power_w: i16,
    crank_revs: u16,
    crank_event_1024: u16,
) -> [u8; MEASUREMENT_LEN] {
    let mut b = [0u8; MEASUREMENT_LEN];
    b[0..2].copy_from_slice(&FLAGS.to_le_bytes());
    b[2..4].copy_from_slice(&power_w.to_le_bytes());
    b[4..6].copy_from_slice(&crank_revs.to_le_bytes());
    b[6..8].copy_from_slice(&crank_event_1024.to_le_bytes());
    b
}

/// Integrates cadence into cumulative crank revolutions and the timestamp of the last completed
/// revolution, exactly like a real crank sensor. Clients derive cadence from the deltas, so the
/// event time is interpolated to the moment the revolution completed rather than "now".
#[derive(Clone, Debug, Default)]
pub struct CrankModel {
    revs: u16,
    last_event_1024: u16,
    /// Fractional revolution completed so far.
    frac: f32,
    last_ms: Option<u64>,
}

impl CrankModel {
    pub fn advance(&mut self, now_ms: u64, cadence_rpm: f32) {
        let Some(last) = self.last_ms else {
            self.last_ms = Some(now_ms);
            return;
        };
        let dt_ms = now_ms.saturating_sub(last);
        self.last_ms = Some(now_ms);
        if dt_ms == 0 || cadence_rpm <= 0.0 {
            return;
        }
        let revs_per_ms = cadence_rpm / 60_000.0;
        let before = self.frac;
        let total = before + revs_per_ms * dt_ms as f32;
        let whole = crate::math::floor(total);
        if whole >= 1.0 {
            // Time of the last completed revolution, interpolated inside this step.
            let since_last_rev_ms = (total - whole) / revs_per_ms;
            let event_ms = now_ms as f32 - since_last_rev_ms;
            self.last_event_1024 = (event_ms * 1.024) as u64 as u16;
            self.revs = self.revs.wrapping_add(whole as u16);
        }
        self.frac = total - whole;
    }

    pub fn revs(&self) -> u16 {
        self.revs
    }

    pub fn last_event_1024(&self) -> u16 {
        self.last_event_1024
    }

    pub fn reset(&mut self) {
        *self = Self {
            last_ms: self.last_ms,
            ..Self::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_layout() {
        let b = encode_measurement(245, 1000, 2048);
        assert_eq!(u16::from_le_bytes([b[0], b[1]]), 0x0020);
        assert_eq!(i16::from_le_bytes([b[2], b[3]]), 245);
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 1000);
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 2048);
        assert_eq!(feature_bytes(), [0x08, 0, 0, 0]);
    }

    #[test]
    fn negative_power_is_signed() {
        let b = encode_measurement(-5, 0, 0);
        assert_eq!(i16::from_le_bytes([b[2], b[3]]), -5);
    }

    #[test]
    fn crank_cadence_is_recoverable_from_deltas() {
        let mut c = CrankModel::default();
        c.advance(0, 90.0);
        let mut samples = alloc::vec::Vec::new();
        for step in 1..=40u64 {
            c.advance(step * 250, 90.0); // 4 Hz for 10 s
            samples.push((c.revs(), c.last_event_1024()));
        }
        let (r0, t0) = samples[3]; // after 1 s
        let (r1, t1) = samples[39]; // after 10 s
        let drev = r1.wrapping_sub(r0) as f32;
        let dt = t1.wrapping_sub(t0) as f32 / 1024.0;
        let cadence = drev / dt * 60.0;
        assert!((cadence - 90.0).abs() < 3.0, "recovered {cadence} rpm");
    }

    #[test]
    fn no_revolutions_at_zero_cadence() {
        let mut c = CrankModel::default();
        c.advance(0, 0.0);
        c.advance(5_000, 0.0);
        assert_eq!(c.revs(), 0);
    }

    #[test]
    fn counters_wrap() {
        let mut c = CrankModel {
            revs: u16::MAX,
            ..Default::default()
        };
        c.advance(0, 120.0);
        c.advance(1_000, 120.0);
        assert_eq!(c.revs(), 1);
    }
}
