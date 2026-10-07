//! State shared between tasks.
//!
//! Everything runs on one cooperative embassy executor (the `esp_rtos::main` thread-mode
//! executor), so the simulator sits behind a `NoopRawMutex` blocking mutex: no interrupt masking,
//! and `with()` is never held across an `.await`.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex as AsyncMutex;
use embassy_sync::signal::Signal;
use fitsim_core::sim::Simulator;
use portable_atomic::AtomicI32;

use crate::storage::Storage;

pub type SimCell = Mutex<NoopRawMutex, RefCell<Simulator>>;
pub type StorageCell = AsyncMutex<NoopRawMutex, Option<Storage>>;

/// Handle passed to every task.
#[derive(Clone, Copy)]
pub struct Shared {
    sim: &'static SimCell,
    pub storage: &'static StorageCell,
    /// True while the setup access point (captive portal) is active.
    pub portal: bool,
}

impl Shared {
    pub fn new(sim: &'static SimCell, storage: &'static StorageCell, portal: bool) -> Self {
        Self {
            sim,
            storage,
            portal,
        }
    }

    /// Runs `f` with exclusive access to the simulator. Never nest calls or `.await` inside `f`.
    pub fn with<R>(&self, f: impl FnOnce(&mut Simulator) -> R) -> R {
        self.sim.lock(|cell| f(&mut cell.borrow_mut()))
    }
}

/// One Wi-Fi network from a scan.
pub struct Network {
    pub ssid: String,
    pub rssi: i8,
    pub secure: bool,
}

pub type ScanResult = Result<Vec<Network>, &'static str>;

/// HTTP handlers ask the Wi-Fi task for a scan through these (the controller is owned by it).
pub static SCAN_REQUEST: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static SCAN_RESULT: Signal<CriticalSectionRawMutex, ScanResult> = Signal::new();
/// Serialises scan requests from concurrent HTTP workers.
pub static SCAN_LOCK: AsyncMutex<CriticalSectionRawMutex, ()> = AsyncMutex::new(());

/// Latest station RSSI, published by the Wi-Fi task. `i32::MIN` = unknown.
pub static WIFI_RSSI: AtomicI32 = AtomicI32::new(i32::MIN);
