//! Hardware-independent core of the ESP32 BLE fitness simulator.
//!
//! Everything in here is `no_std + alloc` and unit-tested on the host. The firmware crate only
//! adds radio / flash / socket plumbing on top; the `fitsim-host` crate runs the very same core
//! on a desktop so the web UI and REST API can be developed without hardware.
//!
//! ```text
//! Web UI ─┐
//! REST ───┼──▶ Simulator (single source of truth) ──▶ BLE services (FTMS / HRS / CPS)
//! Scenario┘                  ▲
//!                            └── FTMS Control Point ◀── Zwift / MyWhoosh
//! ```

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

pub mod api;
pub mod cps;
pub mod event;
pub mod ftms;
pub mod generator;
pub mod hrs;
pub mod http;
pub mod math;
pub mod metric;
pub mod net;
pub mod rng;
pub mod settings;
pub mod sim;
pub mod trainer;
pub mod ws;

/// Firmware / API version reported by `GET /api/system`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
