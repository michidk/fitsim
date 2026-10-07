//! The five live metrics the simulator can generate and the BLE services transmit.

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Metric {
    Speed,
    Cadence,
    Power,
    HeartRate,
    Resistance,
}

impl Metric {
    pub const ALL: [Metric; 5] = [
        Metric::Speed,
        Metric::Cadence,
        Metric::Power,
        Metric::HeartRate,
        Metric::Resistance,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    /// Allowed range of the *simulated* value (what the UI slider spans).
    pub const fn range(self) -> (f32, f32) {
        match self {
            Metric::Speed => (0.0, 120.0),
            Metric::Cadence => (0.0, 200.0),
            Metric::Power => (0.0, 2500.0),
            Metric::HeartRate => (0.0, 250.0),
            Metric::Resistance => (0.0, 100.0),
        }
    }

    pub const fn unit(self) -> &'static str {
        match self {
            Metric::Speed => "km/h",
            Metric::Cadence => "rpm",
            Metric::Power => "W",
            Metric::HeartRate => "bpm",
            Metric::Resistance => "%",
        }
    }

    /// URL / CLI slug (`heart-rate`).
    pub const fn slug(self) -> &'static str {
        match self {
            Metric::Speed => "speed",
            Metric::Cadence => "cadence",
            Metric::Power => "power",
            Metric::HeartRate => "heart-rate",
            Metric::Resistance => "resistance",
        }
    }

    pub fn from_slug(s: &str) -> Option<Metric> {
        match s {
            "speed" => Some(Metric::Speed),
            "cadence" => Some(Metric::Cadence),
            "power" => Some(Metric::Power),
            "heart-rate" | "heartRate" | "hr" => Some(Metric::HeartRate),
            "resistance" => Some(Metric::Resistance),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Metric::Speed => "Speed",
            Metric::Cadence => "Cadence",
            Metric::Power => "Power",
            Metric::HeartRate => "Heart rate",
            Metric::Resistance => "Resistance",
        }
    }

    pub fn clamp(self, v: f32) -> f32 {
        let (lo, hi) = self.range();
        if v.is_nan() { lo } else { v.clamp(lo, hi) }
    }
}

/// The three simulated BLE devices.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Device {
    Trainer,
    HeartRate,
    PowerMeter,
}

impl Device {
    pub const ALL: [Device; 3] = [Device::Trainer, Device::HeartRate, Device::PowerMeter];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Device::Trainer => "Trainer",
            Device::HeartRate => "Heart Rate",
            Device::PowerMeter => "Power Meter",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_roundtrip() {
        for m in Metric::ALL {
            assert_eq!(Metric::from_slug(m.slug()), Some(m));
        }
        assert_eq!(Metric::from_slug("bogus"), None);
    }

    #[test]
    fn clamp_handles_nan() {
        assert_eq!(Metric::Power.clamp(f32::NAN), 0.0);
        assert_eq!(Metric::Power.clamp(9999.0), 2500.0);
    }
}
