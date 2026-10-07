//! Bluetooth SIG **Heart Rate Service** (HRS 1.0).

pub const SERVICE_UUID: u16 = 0x180D;

pub const MAX_RR_PER_PACKET: usize = 4;
pub const MEASUREMENT_MAX_LEN: usize = 2 + 2 * MAX_RR_PER_PACKET;

/// Encodes a Heart Rate Measurement: 8-bit heart rate, sensor-contact feature supported,
/// contact detected iff `bpm > 0`, plus any RR intervals (units of 1/1024 s) accumulated since
/// the previous notification.
pub fn encode_measurement(bpm: u8, rr_1024: &[u16]) -> crate::ftms::Packet<MEASUREMENT_MAX_LEN> {
    let rr = &rr_1024[..rr_1024.len().min(MAX_RR_PER_PACKET)];
    let mut flags = 0b0000_0100; // bit 2: contact feature supported
    if bpm > 0 {
        flags |= 0b0000_0010; // bit 1: contact detected
    }
    if !rr.is_empty() {
        flags |= 0b0001_0000; // bit 4: RR-Interval present
    }
    let mut bytes = [0u8; MEASUREMENT_MAX_LEN];
    bytes[0] = flags;
    bytes[1] = bpm;
    let mut len = 2;
    for v in rr {
        bytes[len..len + 2].copy_from_slice(&v.to_le_bytes());
        len += 2;
    }
    crate::ftms::Packet { bytes, len }
}

/// Tracks heart beats between notifications so RR intervals are consistent with the simulated
/// heart rate (a real strap reports every beat since the last notification).
#[derive(Clone, Debug, Default)]
pub struct BeatTracker {
    /// Time already elapsed within the current beat.
    accum_s: f32,
}

impl BeatTracker {
    /// Advances by `dt_s` at `bpm` and returns the completed beats as RR intervals in 1/1024 s.
    pub fn advance(&mut self, dt_s: f32, bpm: f32) -> ([u16; MAX_RR_PER_PACKET], usize) {
        let mut out = [0u16; MAX_RR_PER_PACKET];
        if bpm < 20.0 {
            self.accum_s = 0.0;
            return (out, 0);
        }
        let interval = 60.0 / bpm;
        self.accum_s += dt_s.clamp(0.0, 10.0);
        let mut n = 0;
        while self.accum_s >= interval {
            self.accum_s -= interval;
            if n < MAX_RR_PER_PACKET {
                out[n] = crate::math::round(interval * 1024.0) as u16;
                n += 1;
            }
        }
        (out, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_without_rr() {
        let p = encode_measurement(145, &[]);
        assert_eq!(p.as_slice(), [0b0000_0110, 145]);
    }

    #[test]
    fn measurement_with_rr_and_no_contact_at_zero() {
        let p = encode_measurement(0, &[1024, 512]);
        assert_eq!(p.as_slice(), [0b0001_0100, 0, 0x00, 0x04, 0x00, 0x02]);
    }

    #[test]
    fn rr_is_capped() {
        let p = encode_measurement(60, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(p.len, 2 + 2 * MAX_RR_PER_PACKET);
    }

    #[test]
    fn beat_tracker_matches_heart_rate() {
        let mut t = BeatTracker::default();
        let mut beats = 0;
        let mut rr_sum = 0u32;
        for _ in 0..60 {
            let (rr, n) = t.advance(1.0, 120.0);
            beats += n;
            rr_sum += rr[..n].iter().map(|&x| x as u32).sum::<u32>();
        }
        // 120 bpm for 60 s is 120 beats.
        assert!((119..=121).contains(&beats), "{beats}");
        // each RR is 0.5 s = 512 units
        assert_eq!(rr_sum / beats as u32, 512);
        assert_eq!(t.advance(1.0, 0.0).1, 0);
    }
}
