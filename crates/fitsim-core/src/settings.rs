//! Persistent device settings and Wi-Fi credentials.

use alloc::string::String;
use serde::{Deserialize, Serialize};

pub const MAX_NAME_LEN: usize = 24;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub hostname: String,
    /// The advertised BLE name (one device carries the trainer, heart-rate and power services).
    pub device_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hostname: "fitness-simulator".into(),
            device_name: "DebugTrainer".into(),
        }
    }
}

/// RFC 1123 host label: lowercase letters, digits and hyphens, not starting/ending with a hyphen.
pub fn validate_hostname(h: &str) -> Result<(), String> {
    let ok = !h.is_empty()
        && h.len() <= 32
        && !h.starts_with('-')
        && !h.ends_with('-')
        && h.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err("hostname must be 1..=32 chars of a-z, 0-9 and '-'".into())
    }
}

impl Settings {
    pub fn sanitize(&mut self) -> Result<(), String> {
        validate_hostname(&self.hostname)?;
        let n = &self.device_name;
        if n.is_empty() || n.len() > MAX_NAME_LEN {
            return Err(alloc::format!(
                "deviceName must be 1..={MAX_NAME_LEN} bytes"
            ));
        }
        if n.chars().any(|c| c.is_control() || !c.is_ascii()) {
            return Err("deviceName must be printable ASCII".into());
        }
        Ok(())
    }
}

/// Deep-merges `patch` into `base` (objects recursively, everything else replaced) so a partial
/// `PUT /api/settings` only touches the fields it mentions.
pub fn merge_json(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(b), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                merge_json(b.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (b, p) => *b = p,
    }
}

/// Wi-Fi station credentials. Stored separately and never returned by the API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiConfig {
    pub ssid: String,
    #[serde(default)]
    pub password: String,
}

impl WifiConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.ssid.is_empty() || self.ssid.len() > 32 {
            return Err("ssid must be 1..=32 bytes".into());
        }
        let pw = self.password.len();
        if pw != 0 && !(8..=63).contains(&pw) {
            return Err("password must be empty (open network) or 8..=63 bytes".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let s = Settings::default();
        assert_eq!(s.hostname, "fitness-simulator");
        assert_eq!(s.device_name, "DebugTrainer");
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s: Settings = serde_json::from_str(r#"{"hostname":"desk"}"#).unwrap();
        assert_eq!(s.hostname, "desk");
        assert_eq!(s.device_name, "DebugTrainer");
    }

    #[test]
    fn sanitize_validates() {
        assert!(Settings::default().sanitize().is_ok());
        let mut bad = Settings {
            hostname: "Bad Host".into(),
            ..Default::default()
        };
        assert!(bad.sanitize().is_err());
        bad = Settings {
            device_name: "x".repeat(MAX_NAME_LEN + 1),
            ..Default::default()
        };
        assert!(bad.sanitize().is_err());
        bad = Settings {
            device_name: "Ünïcode".into(),
            ..Default::default()
        };
        assert!(bad.sanitize().is_err());
    }

    #[test]
    fn merge_is_deep() {
        let mut base = serde_json::to_value(Settings::default()).unwrap();
        merge_json(&mut base, serde_json::json!({"deviceName":"Kickr"}));
        let s: Settings = serde_json::from_value(base).unwrap();
        assert_eq!(s.device_name, "Kickr");
        assert_eq!(s.hostname, "fitness-simulator");
    }

    #[test]
    fn wifi_validation() {
        let w = |s: &str, p: &str| WifiConfig {
            ssid: s.into(),
            password: p.into(),
        };
        assert!(w("home", "hunter22").validate().is_ok());
        assert!(w("open", "").validate().is_ok());
        assert!(w("", "").validate().is_err());
        assert!(w("x", "short").validate().is_err());
    }
}
