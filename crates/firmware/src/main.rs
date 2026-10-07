//! ESP32 BLE fitness simulator firmware.
//!
//! Boot: storage → simulator → Wi-Fi (station or provisioning AP) → BLE → HTTP workers → clock.
//! See the repository README for the architecture and `fitsim-core` for the simulation itself.

#![no_std]
#![no_main]

extern crate alloc;

mod ble;
mod http;
mod net;
mod shared;
mod storage;
mod system;
mod workarounds;

use alloc::format;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::mutex::Mutex as AsyncMutex;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::efuse::base_mac_address;
use esp_hal::ram;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_storage::FlashStorage;
use fitsim_core::event::{EventLog, Kind, Level};
use fitsim_core::settings::Settings;
use fitsim_core::sim::Simulator;
use static_cell::StaticCell;

use crate::shared::{Shared, SimCell, StorageCell};
use crate::storage::{FLAG_PROVISION, Storage};

esp_bootloader_esp_idf::esp_app_desc!();

// Heap sizes per chip as (reclaimed, main). The radio stacks need roughly 90 KiB between them; the
// rest is for the simulator, HTTP buffers and JSON. `reclaimed` is RAM the ROM bootloader no
// longer needs. The classic ESP32 has the least RAM: the main stack is whatever static DRAM is
// left after the heap, so every KiB of heap costs a KiB of stack. 36 KiB (100 KiB in total, the
// budget esp-hal's own Wi-Fi + BLE examples use) leaves ~25 KiB of stack; the linker overflows
// above ~59 KiB.
#[cfg(feature = "esp32")]
const HEAP_SIZES: (usize, usize) = (64 * 1024, 36 * 1024);
#[cfg(feature = "esp32s3")]
const HEAP_SIZES: (usize, usize) = (64 * 1024, 150 * 1024);
#[cfg(feature = "esp32c3")]
const HEAP_SIZES: (usize, usize) = (64 * 1024, 90 * 1024);
#[cfg(feature = "esp32c6")]
const HEAP_SIZES: (usize, usize) = (64 * 1024, 110 * 1024);

/// Events kept for the log page. Smaller on the classic ESP32 to bound JSON response sizes.
#[cfg(feature = "esp32")]
const EVENT_LOG_CAPACITY: usize = 100;
#[cfg(not(feature = "esp32"))]
const EVENT_LOG_CAPACITY: usize = fitsim_core::event::DEFAULT_CAPACITY;

static SIM: StaticCell<SimCell> = StaticCell::new();
static STORAGE: StaticCell<StorageCell> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger(log::LevelFilter::Info);
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: HEAP_SIZES.0);
    esp_alloc::heap_allocator!(size: HEAP_SIZES.1);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);
    log::info!(
        "fitness simulator {} on {}",
        fitsim_core::VERSION,
        system::CHIP
    );

    // Persistent state.
    let mut storage = Storage::open(FlashStorage::new(peripherals.FLASH));
    let (settings, wifi, flags) = match storage.as_mut() {
        Some(st) => (
            st.load_settings().await,
            st.load_wifi().await,
            st.flags().await,
        ),
        None => (Settings::default(), None, 0),
    };
    // The "provision next boot" request is one-shot.
    let force_provisioning = flags & FLAG_PROVISION != 0;
    if force_provisioning && let Some(st) = storage.as_mut() {
        let _ = st.set_flags(flags & !FLAG_PROVISION).await;
    }

    let rng = Rng::new();
    let seed = rng.random();
    let mut sim = Simulator::new(settings.clone(), seed);
    sim.log = EventLog::new(EVENT_LOG_CAPACITY);
    sim.platform.chip = system::CHIP.into();
    sim.platform.heap_total = (HEAP_SIZES.0 + HEAP_SIZES.1) as u32;
    sim.log(
        Level::Info,
        Kind::System,
        format!(
            "Firmware {} started on {}",
            fitsim_core::VERSION,
            system::CHIP
        ),
    );
    if storage.is_none() {
        sim.log(
            Level::Warn,
            Kind::System,
            "No storage partition found: settings will not persist",
        );
    }

    let has_saved = wifi.is_some();
    let mode = match wifi {
        Some(creds) if !force_provisioning => net::Mode::Station(creds),
        _ => net::Mode::Provision { has_saved },
    };
    let portal = matches!(mode, net::Mode::Provision { .. });

    let shared = Shared::new(
        SIM.init(Mutex::new(core::cell::RefCell::new(sim))),
        STORAGE.init(AsyncMutex::new(storage)),
        portal,
    );

    let mac: [u8; 6] = base_mac_address()
        .as_bytes()
        .try_into()
        .unwrap_or([0x02, 0, 0, 0, 0, 1]);
    let seed64 = (rng.random() as u64) << 32 | rng.random() as u64;
    let stack = net::start(
        spawner,
        peripherals.WIFI,
        shared,
        mode,
        settings.hostname.clone(),
        seed64,
    );
    ble::start(spawner, peripherals.BT, shared, mac);

    for _ in 0..http::WORKERS {
        spawner.spawn(http::worker(stack, shared).unwrap());
    }
    spawner.spawn(system::tick_task(shared, stack).unwrap());

    log::info!("running; web UI on port 80");
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}
