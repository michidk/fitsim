//! Simulation clock, platform status and execution of API side effects (persist, reboot).

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Ticker, Timer};
use fitsim_core::api::Effect;
use fitsim_core::event::{Kind, Level};
use fitsim_core::sim::TICK_MS;
use portable_atomic::Ordering;

use crate::shared::{Shared, WIFI_RSSI};
use crate::storage::FLAG_PROVISION;

pub const CHIP: &str = if cfg!(feature = "esp32") {
    "ESP32"
} else if cfg!(feature = "esp32s3") {
    "ESP32-S3"
} else if cfg!(feature = "esp32c3") {
    "ESP32-C3"
} else if cfg!(feature = "esp32c6") {
    "ESP32-C6"
} else {
    "unknown"
};

/// Advances the simulation every [`TICK_MS`] and refreshes platform info (heap, IP, RSSI) once a second.
#[embassy_executor::task]
pub async fn tick_task(shared: Shared, stack: Stack<'static>) {
    let mut ticker = Ticker::every(Duration::from_millis(TICK_MS));
    let mut last_info = 0u64;
    loop {
        ticker.next().await;
        let now = Instant::now().as_millis();
        shared.with(|s| s.tick(now));
        if now.saturating_sub(last_info) >= 1000 {
            last_info = now;
            let ip = stack.config_v4().map(|c| c.address.address().to_string());
            let rssi = WIFI_RSSI.load(Ordering::Relaxed);
            let free = esp_alloc::HEAP.free() as u32;
            let used = esp_alloc::HEAP.used() as u32;
            shared.with(|s| {
                s.platform.chip = CHIP.into();
                s.platform.heap_free = free;
                s.platform.heap_total = free + used;
                if s.platform.wifi.mode == "station" {
                    s.platform.wifi.ip = ip.unwrap_or_default();
                    s.platform.wifi.rssi = (rssi != i32::MIN).then_some(rssi);
                }
            });
        }
    }
}

pub async fn reboot() -> ! {
    // Give the HTTP response / log lines a moment to leave the socket.
    Timer::after(Duration::from_millis(400)).await;
    esp_hal::system::software_reset()
}

/// Remembers (in flash) that the next boot should open the setup access point, then reboots.
pub async fn reboot_into_provisioning(shared: Shared) -> ! {
    if let Some(storage) = shared.storage.lock().await.as_mut() {
        let flags = storage.flags().await | FLAG_PROVISION;
        let _ = storage.set_flags(flags).await;
    }
    reboot().await
}

fn report(shared: Shared, what: &str, result: Result<(), &'static str>) {
    if let Err(e) = result {
        shared.with(|s| {
            s.log(
                Level::Error,
                Kind::System,
                format!("Could not save {what}: {e}"),
            )
        });
    }
}

/// Runs the side effects requested by an API call (after its response was sent).
pub async fn apply_effects(shared: Shared, effects: Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::PersistSettings => {
                let settings = shared.with(|s| s.settings.clone());
                let mut guard = shared.storage.lock().await;
                match guard.as_mut() {
                    Some(st) => report(shared, "settings", st.save_settings(&settings).await),
                    None => report(shared, "settings", Err("no storage partition")),
                }
            }
            Effect::SaveWifi(cfg) => {
                let mut guard = shared.storage.lock().await;
                if let Some(st) = guard.as_mut() {
                    report(shared, "Wi-Fi credentials", st.save_wifi(&cfg).await);
                    // A fresh configuration replaces any pending "go to setup" request.
                    let flags = st.flags().await & !FLAG_PROVISION;
                    let _ = st.set_flags(flags).await;
                } else {
                    report(shared, "Wi-Fi credentials", Err("no storage partition"));
                }
            }
            Effect::FactoryReset => {
                let mut guard = shared.storage.lock().await;
                if let Some(st) = guard.as_mut() {
                    report(shared, "factory reset", st.erase_all().await);
                }
            }
            Effect::Reboot => reboot().await,
        }
    }
}
