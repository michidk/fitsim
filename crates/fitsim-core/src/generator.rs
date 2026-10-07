//! Per-metric value generators: fixed, manual, ramp, oscillation, random variation, random walk
//! and scripted sequences.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::math::{self, lerp};
use crate::rng::Rng;

const TAU: f32 = core::f32::consts::TAU;
pub const MAX_SEQUENCE_POINTS: usize = 64;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Waveform {
    #[default]
    Sine,
    Triangle,
    Square,
    Saw,
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeqPoint {
    /// Seconds since the generator was (re)started.
    pub t: f32,
    pub v: f32,
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
}

/// Generator configuration as exchanged over the REST API (`{"mode":"ramp",...}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Generator {
    /// Constant value, set from a numeric input.
    Fixed { value: f32 },
    /// Constant value, driven live by a slider.
    Manual { value: f32 },
    /// Linear ramp from `start` to `end` over `duration_s`; holds the end value (or repeats).
    Ramp {
        start: f32,
        end: f32,
        duration_s: f32,
        #[serde(default)]
        repeat: bool,
    },
    /// Periodic wave around `center`. Starts at the minimum so 140 → 150 → 160 → 150 → 140
    /// is `center: 150, amplitude: 10`.
    Oscillation {
        center: f32,
        amplitude: f32,
        period_s: f32,
        #[serde(default)]
        waveform: Waveform,
    },
    /// `base ± variation`, re-rolled every `interval_s`.
    RandomVariation {
        base: f32,
        variation: f32,
        #[serde(default = "one")]
        interval_s: f32,
    },
    /// Bounded random walk limited to `max_step_per_s` change per second.
    RandomWalk {
        min: f32,
        max: f32,
        max_step_per_s: f32,
    },
    /// Scripted keyframes (`t` in seconds), optionally interpolated and repeating.
    Sequence {
        points: Vec<SeqPoint>,
        #[serde(default = "yes")]
        interpolate: bool,
        #[serde(default)]
        repeat: bool,
    },
}

impl Generator {
    pub fn mode_name(&self) -> &'static str {
        match self {
            Generator::Fixed { .. } => "fixed",
            Generator::Manual { .. } => "manual",
            Generator::Ramp { .. } => "ramp",
            Generator::Oscillation { .. } => "oscillation",
            Generator::RandomVariation { .. } => "randomVariation",
            Generator::RandomWalk { .. } => "randomWalk",
            Generator::Sequence { .. } => "sequence",
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        fn finite(name: &str, v: f32) -> Result<(), String> {
            if v.is_finite() {
                Ok(())
            } else {
                Err(alloc::format!("{name} must be a finite number"))
            }
        }
        match self {
            Generator::Fixed { value } | Generator::Manual { value } => finite("value", *value),
            Generator::Ramp {
                start,
                end,
                duration_s,
                ..
            } => {
                finite("start", *start)?;
                finite("end", *end)?;
                finite("durationS", *duration_s)?;
                if *duration_s < 0.0 {
                    return Err("durationS must be >= 0".into());
                }
                Ok(())
            }
            Generator::Oscillation {
                center,
                amplitude,
                period_s,
                ..
            } => {
                finite("center", *center)?;
                finite("amplitude", *amplitude)?;
                finite("periodS", *period_s)?;
                if *period_s <= 0.0 {
                    return Err("periodS must be > 0".into());
                }
                Ok(())
            }
            Generator::RandomVariation {
                base,
                variation,
                interval_s,
            } => {
                finite("base", *base)?;
                finite("variation", *variation)?;
                finite("intervalS", *interval_s)?;
                if !(0.05..=3600.0).contains(interval_s) {
                    return Err("intervalS must be between 0.05 and 3600".into());
                }
                Ok(())
            }
            Generator::RandomWalk {
                min,
                max,
                max_step_per_s,
            } => {
                finite("min", *min)?;
                finite("max", *max)?;
                finite("maxStepPerS", *max_step_per_s)?;
                if min > max {
                    return Err("min must be <= max".into());
                }
                if *max_step_per_s < 0.0 {
                    return Err("maxStepPerS must be >= 0".into());
                }
                Ok(())
            }
            Generator::Sequence { points, .. } => {
                if points.is_empty() || points.len() > MAX_SEQUENCE_POINTS {
                    return Err(alloc::format!(
                        "sequence needs 1..={MAX_SEQUENCE_POINTS} points"
                    ));
                }
                let mut prev = 0.0;
                for p in points {
                    finite("t", p.t)?;
                    finite("v", p.v)?;
                    if p.t < 0.0 || p.t < prev {
                        return Err("sequence points must have non-decreasing t >= 0".into());
                    }
                    prev = p.t;
                }
                Ok(())
            }
        }
    }
}

/// Evaluates a keyframe series at time `t`. Holds the first/last value outside the series.
/// With `interpolate == false` the value steps at each keyframe.
pub fn eval_series(points: &[(f32, f32)], t: f32, interpolate: bool) -> f32 {
    match points {
        [] => 0.0,
        [only] => only.1,
        _ => {
            if t <= points[0].0 {
                return points[0].1;
            }
            let last = points[points.len() - 1];
            if t >= last.0 {
                return last.1;
            }
            let mut prev = points[0];
            for &next in &points[1..] {
                if t < next.0 {
                    if !interpolate {
                        return prev.1;
                    }
                    let span = next.0 - prev.0;
                    return if span <= 0.0 {
                        next.1
                    } else {
                        lerp(prev.1, next.1, (t - prev.0) / span)
                    };
                }
                prev = next;
            }
            last.1
        }
    }
}

fn wave(w: Waveform, phase: f32) -> f32 {
    // `phase` in [0, 1). All waves start at -1 and reach +1 at phase 0.5.
    match w {
        Waveform::Sine => -math::cos(phase * TAU),
        Waveform::Triangle => {
            if phase < 0.5 {
                -1.0 + 4.0 * phase
            } else {
                3.0 - 4.0 * phase
            }
        }
        Waveform::Square => {
            if phase < 0.5 {
                -1.0
            } else {
                1.0
            }
        }
        Waveform::Saw => -1.0 + 2.0 * phase,
    }
}

/// A generator plus the state it needs between samples.
#[derive(Clone, Debug)]
pub struct GeneratorRuntime {
    pub config: Generator,
    started_ms: u64,
    walk: f32,
    walk_dir: f32,
    variation_value: f32,
    variation_next_ms: u64,
}

impl GeneratorRuntime {
    pub fn new(config: Generator, now_ms: u64, current: f32) -> Self {
        let mut rt = Self {
            config: Generator::Fixed { value: 0.0 },
            started_ms: now_ms,
            walk: current,
            walk_dir: 0.0,
            variation_value: current,
            variation_next_ms: now_ms,
        };
        rt.set(config, now_ms, current);
        rt
    }

    /// Replace the configuration and restart the generator clock. `current` seeds the random walk
    /// so switching modes does not make the value jump.
    pub fn set(&mut self, config: Generator, now_ms: u64, current: f32) {
        self.started_ms = now_ms;
        self.walk = match &config {
            Generator::RandomWalk { min, max, .. } => current.clamp(*min, *max),
            _ => current,
        };
        self.walk_dir = 0.0;
        self.variation_value = current;
        self.variation_next_ms = now_ms;
        self.config = config;
    }

    /// Seconds since the generator was started.
    pub fn elapsed_s(&self, now_ms: u64) -> f32 {
        now_ms.saturating_sub(self.started_ms) as f32 / 1000.0
    }

    pub fn sample(&mut self, now_ms: u64, dt_s: f32, rng: &mut Rng) -> f32 {
        let t = self.elapsed_s(now_ms);
        match &self.config {
            Generator::Fixed { value } | Generator::Manual { value } => *value,
            Generator::Ramp {
                start,
                end,
                duration_s,
                repeat,
            } => {
                if *duration_s <= 0.0 {
                    *end
                } else {
                    let p = if *repeat {
                        math::fmod(t, *duration_s) / duration_s
                    } else {
                        (t / duration_s).min(1.0)
                    };
                    lerp(*start, *end, p)
                }
            }
            Generator::Oscillation {
                center,
                amplitude,
                period_s,
                waveform,
            } => {
                let phase = math::fmod(t, *period_s) / period_s;
                center + amplitude * wave(*waveform, phase)
            }
            Generator::RandomVariation {
                base,
                variation,
                interval_s,
            } => {
                if now_ms >= self.variation_next_ms {
                    self.variation_value = base + rng.signed() * variation;
                    self.variation_next_ms = now_ms + (interval_s * 1000.0) as u64;
                }
                self.variation_value
            }
            Generator::RandomWalk {
                min,
                max,
                max_step_per_s,
            } => {
                // Momentum keeps the walk drifting in one direction for a while instead of
                // jittering like white noise, while |dir| <= 1 bounds the change rate.
                self.walk_dir = (self.walk_dir * 0.95 + rng.signed() * 0.3).clamp(-1.0, 1.0);
                self.walk += self.walk_dir * max_step_per_s * dt_s;
                if self.walk <= *min {
                    self.walk = *min;
                    self.walk_dir = self.walk_dir.abs();
                } else if self.walk >= *max {
                    self.walk = *max;
                    self.walk_dir = -self.walk_dir.abs();
                }
                self.walk
            }
            Generator::Sequence {
                points,
                interpolate,
                repeat,
            } => {
                let series: Vec<(f32, f32)> = points.iter().map(|p| (p.t, p.v)).collect();
                let end = points.last().map(|p| p.t).unwrap_or(0.0);
                let t = if *repeat && end > 0.0 {
                    math::fmod(t, end)
                } else {
                    t
                };
                eval_series(&series, t, *interpolate)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rt: &mut GeneratorRuntime, secs: f32, step_ms: u64) -> Vec<f32> {
        let mut rng = Rng::new(1);
        let mut out = Vec::new();
        let mut now = rt.started_ms;
        let end = now + (secs * 1000.0) as u64;
        while now <= end {
            out.push(rt.sample(now, step_ms as f32 / 1000.0, &mut rng));
            now += step_ms;
        }
        out
    }

    #[test]
    fn ramp_interpolates_then_holds() {
        let mut rt = GeneratorRuntime::new(
            Generator::Ramp {
                start: 100.0,
                end: 400.0,
                duration_s: 60.0,
                repeat: false,
            },
            0,
            0.0,
        );
        let mut rng = Rng::new(1);
        assert_eq!(rt.sample(0, 0.05, &mut rng), 100.0);
        assert_eq!(rt.sample(30_000, 0.05, &mut rng), 250.0);
        assert_eq!(rt.sample(60_000, 0.05, &mut rng), 400.0);
        assert_eq!(rt.sample(90_000, 0.05, &mut rng), 400.0);
    }

    #[test]
    fn repeating_ramp_wraps() {
        let mut rt = GeneratorRuntime::new(
            Generator::Ramp {
                start: 0.0,
                end: 10.0,
                duration_s: 10.0,
                repeat: true,
            },
            0,
            0.0,
        );
        let mut rng = Rng::new(1);
        assert_eq!(rt.sample(15_000, 0.05, &mut rng), 5.0);
    }

    #[test]
    fn oscillation_matches_spec_example() {
        // center 150, amplitude 10, period 20 s  =>  140 → 150 → 160 → 150 → 140
        let mut rt = GeneratorRuntime::new(
            Generator::Oscillation {
                center: 150.0,
                amplitude: 10.0,
                period_s: 20.0,
                waveform: Waveform::Sine,
            },
            0,
            0.0,
        );
        let mut rng = Rng::new(1);
        let at = |rt: &mut GeneratorRuntime, s: u64, rng: &mut Rng| rt.sample(s * 1000, 0.05, rng);
        assert!((at(&mut rt, 0, &mut rng) - 140.0).abs() < 0.01);
        assert!((at(&mut rt, 5, &mut rng) - 150.0).abs() < 0.01);
        assert!((at(&mut rt, 10, &mut rng) - 160.0).abs() < 0.01);
        assert!((at(&mut rt, 15, &mut rng) - 150.0).abs() < 0.01);
        assert!((at(&mut rt, 20, &mut rng) - 140.0).abs() < 0.01);
    }

    #[test]
    fn other_waveforms_span_the_range() {
        for w in [Waveform::Triangle, Waveform::Square, Waveform::Saw] {
            let mut rt = GeneratorRuntime::new(
                Generator::Oscillation {
                    center: 0.0,
                    amplitude: 1.0,
                    period_s: 4.0,
                    waveform: w,
                },
                0,
                0.0,
            );
            let v = run(&mut rt, 4.0, 50);
            let min = v.iter().cloned().fold(f32::MAX, f32::min);
            let max = v.iter().cloned().fold(f32::MIN, f32::max);
            assert!((min + 1.0).abs() < 0.05, "{w:?} min {min}");
            assert!(max > 0.9, "{w:?} max {max}");
        }
    }

    #[test]
    fn random_variation_holds_between_updates() {
        let mut rt = GeneratorRuntime::new(
            Generator::RandomVariation {
                base: 90.0,
                variation: 5.0,
                interval_s: 1.0,
            },
            0,
            90.0,
        );
        let mut rng = Rng::new(3);
        let first = rt.sample(0, 0.05, &mut rng);
        assert_eq!(rt.sample(500, 0.05, &mut rng), first);
        let mut seen_change = false;
        for s in 1..20u64 {
            let v = rt.sample(s * 1000, 0.05, &mut rng);
            assert!((85.0..=95.0).contains(&v), "{v}");
            seen_change |= v != first;
        }
        assert!(seen_change);
    }

    #[test]
    fn random_walk_is_bounded_and_rate_limited() {
        let mut rt = GeneratorRuntime::new(
            Generator::RandomWalk {
                min: 130.0,
                max: 170.0,
                max_step_per_s: 2.0,
            },
            0,
            150.0,
        );
        let v = run(&mut rt, 600.0, 50);
        for w in v.windows(2) {
            assert!(
                (w[1] - w[0]).abs() <= 2.0 * 0.05 + 1e-4,
                "step {} -> {}",
                w[0],
                w[1]
            );
        }
        assert!(v.iter().all(|x| (130.0..=170.0).contains(x)));
        let spread =
            v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread > 5.0, "walk barely moved: {spread}");
    }

    #[test]
    fn sequence_interpolates_and_repeats() {
        let pts = alloc::vec![
            SeqPoint { t: 0.0, v: 100.0 },
            SeqPoint { t: 10.0, v: 150.0 },
            SeqPoint { t: 20.0, v: 200.0 },
        ];
        let mut rt = GeneratorRuntime::new(
            Generator::Sequence {
                points: pts.clone(),
                interpolate: true,
                repeat: false,
            },
            0,
            0.0,
        );
        let mut rng = Rng::new(1);
        assert_eq!(rt.sample(5_000, 0.05, &mut rng), 125.0);
        assert_eq!(rt.sample(99_000, 0.05, &mut rng), 200.0);

        let mut stepped = GeneratorRuntime::new(
            Generator::Sequence {
                points: pts.clone(),
                interpolate: false,
                repeat: false,
            },
            0,
            0.0,
        );
        assert_eq!(stepped.sample(9_999, 0.05, &mut rng), 100.0);
        assert_eq!(stepped.sample(10_000, 0.05, &mut rng), 150.0);

        let mut looping = GeneratorRuntime::new(
            Generator::Sequence {
                points: pts,
                interpolate: true,
                repeat: true,
            },
            0,
            0.0,
        );
        // 25 s into a 20 s loop = 5 s, halfway between 100 and 150.
        assert_eq!(looping.sample(25_000, 0.05, &mut rng), 125.0);
    }

    #[test]
    fn json_roundtrip_uses_camel_case_tags() {
        let g: Generator =
            serde_json::from_str(r#"{"mode":"ramp","start":100,"end":400,"durationS":60}"#)
                .unwrap();
        assert_eq!(
            g,
            Generator::Ramp {
                start: 100.0,
                end: 400.0,
                duration_s: 60.0,
                repeat: false
            }
        );
        let g: Generator =
            serde_json::from_str(r#"{"mode":"randomWalk","min":130,"max":170,"maxStepPerS":2}"#)
                .unwrap();
        assert!(matches!(g, Generator::RandomWalk { .. }));
    }

    #[test]
    fn validation_rejects_nonsense() {
        assert!(
            Generator::Oscillation {
                center: 1.0,
                amplitude: 1.0,
                period_s: 0.0,
                waveform: Waveform::Sine
            }
            .validate()
            .is_err()
        );
        assert!(
            Generator::RandomWalk {
                min: 5.0,
                max: 1.0,
                max_step_per_s: 1.0
            }
            .validate()
            .is_err()
        );
        assert!(Generator::Fixed { value: f32::NAN }.validate().is_err());
        assert!(
            Generator::Sequence {
                points: Vec::new(),
                interpolate: true,
                repeat: false
            }
            .validate()
            .is_err()
        );
        assert!(
            Generator::Sequence {
                points: alloc::vec![SeqPoint { t: 5.0, v: 1.0 }, SeqPoint { t: 1.0, v: 1.0 }],
                interpolate: true,
                repeat: false
            }
            .validate()
            .is_err()
        );
        assert!(Generator::Manual { value: 3.0 }.validate().is_ok());
    }
}
