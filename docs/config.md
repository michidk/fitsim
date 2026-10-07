# Configuration and tuning

The runtime settings (hostname and BLE name) live in the web UI and are stored in
flash; their defaults are in `crates/fitsim-core/src/settings.rs`. This file lists the
**compile-time** settings of the firmware.

## Flash layout (`crates/firmware/partitions.csv`)

| Partition | Offset | Size | Purpose |
|---|---|---|---|
| `nvs` | `0x9000` | 16 KiB | unused (kept for tool compatibility) |
| `phy_init` | `0xd000` | 4 KiB | unused |
| `factory` | `0x10000` | 1.75 MiB | application |
| `storage` | `0x1D0000` | 128 KiB | settings and Wi-Fi credentials |

The table ends at 0x1F0000, so it fits 2 MB flash. The firmware finds the `storage` partition by
name; storage keys are `1` settings, `2` Wi-Fi, `3` flags.

## Memory (`crates/firmware/src/main.rs`)

| Chip | Reclaimed heap | Main heap |
|---|---|---|
| ESP32 | 64 KiB | 36 KiB |
| ESP32-S3 | 64 KiB | 150 KiB |
| ESP32-C3 | 64 KiB | 90 KiB |
| ESP32-C6 | 64 KiB | 110 KiB |

The main task stack is the static RAM left after the heap, so on the ESP32 every KiB of heap costs a
KiB of stack, and the linker fails above ~59 KiB of main heap there. The radios need about 90 KiB of
heap between them. `heapFree` / `heapTotal` are reported by `GET /api/system`.

## HTTP server (`crates/firmware/src/http.rs`)

| Constant | ESP32 | others | Effect |
|---|---|---|---|
| `WORKERS` | 3 | 5 | concurrent connections (each open dashboard holds one) |
| `MAX_WEBSOCKETS` | 1 | 2 | dashboards open at once; the rest keep the REST API reachable |
| `RX_BUF` / `TX_BUF` | 1024 / 1536 | 1536 / 2048 | TCP socket buffers per worker (heap) |

Request limits are shared with the host build in `crates/fitsim-core/src/http.rs`
(`MAX_HEAD_BYTES` 2048, `MAX_BODY_BYTES` 8192). `EVENT_LOG_CAPACITY` in `main.rs` is the number of
log entries kept (100 on the ESP32, 300 otherwise).

## Wi-Fi and BLE (`net.rs`, `ble.rs`)

| Constant | Value | Effect |
|---|---|---|
| `MAX_STA_FAILURES` | 24 | failed connection attempts (about 5 s apart) before rebooting into setup mode |
| `PROVISION_TIMEOUT` | 15 min | setup mode without a client before retrying the saved network |
| `MAX_CONNECTIONS` | 3 | simultaneous BLE clients (also `connections_max` on the `gatt_server` and the connection task pool size) |

## Simulation (`crates/fitsim-core/src/sim.rs`)

`TICK_MS` 50 (20 Hz simulation step) and `notify_interval_ms` (4 Hz trainer and power, 1 Hz heart
rate). Metric ranges (speed 0-120, cadence 0-200, power 0-2500, heart rate 0-250, resistance 0-100) are in
`metric.rs`.

## Optimisation level

`crates/firmware/Cargo.toml` builds everything for size (`opt-level = "s"`, fat LTO) except
`esp-radio`, which needs at least `-O2` (its build script rejects lower levels).
