//! Thin `libm` wrappers so the rest of the crate can stay `no_std` without sprinkling `libm::`.

#[inline]
pub fn cos(x: f32) -> f32 {
    libm::cosf(x)
}

#[inline]
pub fn floor(x: f32) -> f32 {
    libm::floorf(x)
}

#[inline]
pub fn round(x: f32) -> f32 {
    libm::roundf(x)
}

#[inline]
pub fn fmod(x: f32, y: f32) -> f32 {
    libm::fmodf(x, y)
}

/// Round to `decimals` decimal places (used to keep JSON telemetry compact).
#[inline]
pub fn round_to(x: f32, decimals: u32) -> f32 {
    let scale = libm::powf(10.0, decimals as f32);
    round(x * scale) / scale
}

/// Linear interpolation, `t` in `0.0..=1.0`.
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_to_decimals() {
        assert_eq!(round_to(1.2345, 2), 1.23);
        assert_eq!(round_to(-1.2355, 1), -1.2);
    }
}
