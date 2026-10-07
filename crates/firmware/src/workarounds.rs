//! Workarounds for upstream issues. Remove when the linked issue is fixed.

/// <https://github.com/esp-rs/esp-hal/issues/6408>: the classic-ESP32 Bluetooth blob shipped with
/// `esp-radio 1.0.0-beta.1` references three Classic-BT inquiry/page scan callbacks that no
/// archive or linker script provides (`libbtdm_app.a(ea.o)`), so the link fails.
///
/// This firmware is BLE-only and never starts Classic Bluetooth scanning, so the callbacks are
/// never invoked; empty definitions satisfy the linker. Other chips are unaffected.
#[cfg(feature = "esp32")]
mod btdm_classic_stubs {
    #[unsafe(no_mangle)]
    pub extern "C" fn ld_iscan_evt_start_cbk() {}

    #[unsafe(no_mangle)]
    pub extern "C" fn ld_page_evt_start_cbk() {}

    #[unsafe(no_mangle)]
    pub extern "C" fn ld_pscan_evt_start_cbk() {}
}
