# ESP32 BLE Fitness Simulator

A desk-sized **BLE fitness test bench**. An ESP32 pretends to be a smart trainer (FTMS), a heart-rate strap and a cycling power meter, so you can develop and test Zwift, MyWhoosh or your own cycling app **without real exercise equipment**.

Open `http://fitness-simulator.local`, move a slider, and the value changes on the BLE link a moment later. See exactly what the app asks the trainer to do.

> **Status.** The simulation core, REST/WebSocket API, web UI and network protocol code are covered by over 100 unit and end-to-end tests and run identically on your desktop (`fitsim-host`). The firmware builds, lints and links for ESP32, ESP32-S3, ESP32-C3 and ESP32-C6. **It has not been run on real hardware or against Zwift/MyWhoosh yet.**

## Features

- **One BLE device, three services**: Fitness Machine (indoor bike), Heart Rate and Cycling Power, always on, advertised together under one name (default `DebugTrainer`).
- **FTMS Control Point**: request control, reset, start/stop/pause, **target power (ERG)**, target resistance, **indoor bike simulation** (grade, wind, Crr, Cw). Every command is shown live and logged.
- **Live dashboard** over WebSocket: speed, cadence, power, heart rate, resistance, the road parameters the app sends, connection details (MTU, RSSI, subscriptions).
- **Value generators** for every metric: fixed, manual, ramp, oscillation, random variation, random walk, and scripted sequence.
- **ERG**: when the app sets a target power, the simulated trainer reports exactly that power (and resistance targets likewise); the target is shown on the dashboard.
- **Wi-Fi provisioning** through a captive-portal access point, mDNS (`fitness-simulator.local`), settings stored in flash.
- **REST and WebSocket APIs** for scripting ([docs/api.md](docs/api.md)).

## Supported hardware

Any ESP32-class board with Wi-Fi and BLE and 4 MB flash (the partition table also fits 2 MB).

| Chip | Target | Notes |
|---|---|---|
| ESP32-S3 | `xtensa-esp32s3-none-elf` | most RAM, recommended |
| ESP32-C3 | `riscv32imc-unknown-none-elf` | |
| ESP32-C6 | `riscv32imac-unknown-none-elf` | |
| ESP32 | `xtensa-esp32-none-elf` | least RAM, see [limitations](#known-limitations) |

ESP32-S2 has no Bluetooth; ESP32-H2 has no Wi-Fi. Both are unsupported.

## Try it without hardware

The desktop build runs the real simulator, REST router, WebSocket stream and web UI, plus two debug endpoints that stand in for a BLE client.

```sh
cargo run -p fitsim-host -- --port 8080        # add --provisioning to preview the Wi-Fi setup screen
open http://localhost:8080

curl -X POST localhost:8080/api/debug/ble -d '{"event":"connect","client":64,"mtu":247}'
curl -X POST localhost:8080/api/debug/ftms-write -d '{"client":64,"hex":"00"}'       # Request Control
curl -X POST localhost:8080/api/debug/ftms-write -d '{"client":64,"hex":"05b400"}'   # Set Target Power 180 W
```

## Build and flash

You need Rust and [`espflash`](https://github.com/esp-rs/espflash). The Xtensa chips (ESP32, ESP32-S3) also need the `esp` toolchain from [`espup`](https://github.com/esp-rs/espup).

```sh
cargo install espflash --locked

# RISC-V chips (stable Rust; rust-toolchain.toml installs everything)
cargo esp32c3          # build            (or esp32c6)
cargo run-esp32c3      # build, flash, serial monitor

# Xtensa chips
cargo install espup --locked && espup install     # once
. ~/export-esp.sh                                  # in every new shell
cargo +esp esp32s3     # build            (or esp32)
cargo +esp run-esp32s3 # build, flash, serial monitor
```

The runners flash with `crates/firmware/partitions.csv`, which adds a 128 KiB `storage` partition for settings and Wi-Fi credentials. Always flash with it. The firmware is its own cargo workspace, so `cargo test` at the repository root never touches the ESP toolchain. Compile-time tunables are in [docs/config.md](docs/config.md).

## Wi-Fi setup

1. First boot (no credentials): the device opens the access point **`FitnessSimulator-XXXX`** (open, `192.168.4.1`). Join it; a captive portal should open the setup page, otherwise browse to `http://192.168.4.1`.
2. Pick your network, enter the password, press **Connect**. The credentials are stored and the device reboots into station mode.
3. Open **`http://fitness-simulator.local`** or the IP address from your router. The hostname is configurable in **Settings** and applies after a reboot.

If the saved network cannot be reached for a few minutes, the device reboots into the setup access point once, and returns to the saved network after 15 minutes without a client. **Settings -> Factory reset** erases everything.

## Web UI

| Page | What it does |
|---|---|
| **Dashboard** | BLE connections, a Trainer (FTMS) card with control owner, last command, targets and the road parameters the app sent, live values, and per-metric sliders with a generator editor. |
| **Event Log** | Everything that happened, including every FTMS command with its details; filters, clear and download. |
| **Settings** | Wi-Fi, hostname, BLE name, reboot, factory reset, firmware info. |

The frontend is plain HTML/CSS/ES modules in `web/` (about 54 KB, 21 KB gzipped), with no framework and no build step. `crates/fitsim-web` gzips it at build time and embeds it in the firmware.

## Connecting apps

Apps find the simulator by its standard service UUIDs.

- **Zwift**: on the pairing screen select **`DebugTrainer`** as *Power Source* and *Controllable*, and again as *Heart Rate*.
- **MyWhoosh and others**: pick `DebugTrainer` in the trainer, power and heart-rate lists.
- **Anything else**: [nRF Connect](https://www.nordicsemi.com/Products/Development-tools/nRF-Connect-for-mobile) shows the services and lets you subscribe by hand.

These pairing flows follow the Bluetooth profiles but have **not** been verified against Zwift or MyWhoosh yet.

## REST API

Full reference in [docs/api.md](docs/api.md).

```sh
curl localhost/api/state                                          # everything in one snapshot
curl -X POST localhost/api/state/power       -d '{"value": 250}'
curl -X POST localhost/api/state/heart-rate  -d '{"value": 155}'
curl -X PUT  localhost/api/metrics/heart-rate \
     -d '{"mode":"oscillation","center":150,"amplitude":10,"periodS":20}'
curl localhost/api/ftms/control                                   # owner, targets, last commands
curl localhost/api/events.txt                                     # the event log as text
```

Errors are `{"error": "..."}` with a meaningful status. CORS is open and there is **no authentication**: keep the device on a network you trust.

## WebSocket API

`ws://<host>/ws` pushes one JSON object per frame with `type` and `timestamp` (device uptime, ms): `hello`, `state`, `telemetry` (5 Hz), `log`, `ftms-command`, `ble-state`, `log-cleared`.

```json
{ "type": "telemetry", "timestamp": 12345678,
  "data": { "power": 248, "cadence": 92, "heartRate": 153, "speed": 31.8, "resistance": 42, "targetPower": 250 } }
{ "type": "ftms-command", "timestamp": 12224, "command": "setTargetPower", "value": 250,
  "data": { "name": "Set Target Power", "result": "success", "client": 64, "raw": "05fa00" } }
```

## Trainer behaviour (FTMS)

| Op code | Behaviour |
|---|---|
| `0x00` Request Control | Granted if free (or already yours); `Control Not Permitted` if another client owns it. Released when that client disconnects. |
| `0x01` Reset | Clears targets and simulation parameters. Keeps control. |
| `0x04` Set Target Resistance Level | `0..=100`. Replaces a power target. |
| `0x05` Set Target Power | `0..=2000` W. Starts ERG: the reported power equals the target. |
| `0x07` / `0x08` Start, Stop or Pause | Tracks the machine state and sends the status notification. Stop also clears targets. |
| `0x11` Set Indoor Bike Simulation Parameters | Wind, grade, Crr, Cw; shown live on the dashboard (informational: speed is not computed from them). |
| everything else | `Op Code Not Supported`, and still logged. |

Every command gets the `0x80` response indication; successful ones also send a Fitness Machine Status notification. Every command except Request Control needs control first. Indoor Bike Data adapts to the negotiated MTU. Details in [docs/ble.md](docs/ble.md).

## Implemented Bluetooth specifications

| Specification | Service | Characteristics |
|---|---|---|
| Fitness Machine Service 1.0 | `0x1826` | Feature `2ACC`, Indoor Bike Data `2AD2`, Training Status `2AD3`, Supported Resistance Level Range `2AD6`, Supported Power Range `2AD8`, Control Point `2AD9`, Machine Status `2ADA` |
| Heart Rate Service 1.0 | `0x180D` | Heart Rate Measurement `2A37`, Body Sensor Location `2A38` |
| Cycling Power Service 1.1 | `0x1818` | Cycling Power Measurement `2A63`, Feature `2A65`, Sensor Location `2A5D` |
| Device Information Service | `0x180A` | Manufacturer `2A29`, Model `2A24`, Firmware Revision `2A26` |
| GAP / GATT | `0x1800` / `0x1801` | Device Name, Appearance (generic cycling) |

## Architecture

```text
 Web UI ──┐                                  ┌──▶ FTMS trainer ──┐
 REST ────┼──▶ Simulator (one state) ───────┼──▶ Heart rate ────┼──▶ BLE (TrouBLE + esp-radio)
 WebSocket┘            ▲                      └──▶ Power meter ───┘
                       └── Trainer controller ◀── FTMS Control Point ◀── Zwift / MyWhoosh
                              └─▶ Event log ─▶ WebSocket
```

| Crate | Responsibility |
|---|---|
| `fitsim-core` | `no_std + alloc`, hardware independent: simulator, generators, FTMS/HRS/CPS codecs, trainer controller, event log, settings, REST router, HTTP + WebSocket protocol, DHCP/DNS/mDNS packets. Unit tested on the host. |
| `fitsim-web` | Gzips `web/` at build time, embeds it, routes requests (API, static files, captive portal). |
| `fitsim-host` | Desktop build for UI development and end-to-end tests. |
| `crates/firmware` | The ESP32 firmware: radio, flash, sockets and task wiring only. |

Firmware tasks run on one cooperative `embassy` executor on top of `esp-rtos`: `net` (Wi-Fi, DHCP/DNS in setup mode, mDNS), `http` (a worker pool on `embassy-net` sockets), `ble` (TrouBLE host, advertising, one task per connection that sends notifications and answers the Control Point without blocking), `system` (20 Hz simulation clock, side effects), `storage` (wear-levelled key/value store in flash). The BLE layer only reads values and pushes Control Point writes in; the web layer only talks to the simulator.

**Why TrouBLE and not NimBLE?** This is a bare-metal Rust project (no ESP-IDF, like shottimer). NimBLE lives inside ESP-IDF; its Rust bindings need the IDF toolchain and `std`. [TrouBLE](https://github.com/embassy-rs/trouble) is the maintained Rust BLE host for the same ESP controller and integrates with `embassy` and `esp-radio` without a C toolchain. `fitsim-core` has no radio dependency, so only `crates/firmware` would need rewriting for NimBLE.

## Known limitations

- **Not tested on hardware or with real apps.** Please open an issue with the serial log if something does not work.
- **One advertised device.** All three services share one address and name; they cannot appear as three separate devices.
- **At most 3 simultaneous BLE connections. No pairing, bonding or encryption.**
- **The GAP Device Name characteristic updates after a reboot** (the advertised name changes immediately).
- **Wi-Fi and BLE share one radio**, so latency can vary under heavy Wi-Fi traffic. Flash writes (saving settings) briefly stall the CPU.
- **Wi-Fi security**: WPA/WPA2 personal and open networks; no WPA3-only. Credentials are stored unencrypted in flash.
- **Classic ESP32** has little RAM: 3 HTTP workers, one dashboard at a time, a smaller event log. It needs a link-time workaround for an upstream blob bug ([esp-rs/esp-hal#6408](https://github.com/esp-rs/esp-hal/issues/6408), see `crates/firmware/src/workarounds.rs`). Prefer ESP32-S3/C3/C6.
- **No authentication on the web UI and API.**
- `esp-radio` is still a beta (`1.0.0-beta.1`); the ESP dependencies must be bumped together.

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The end-to-end tests start the real server on a free port and talk to it over raw TCP, including the WebSocket handshake. CI also builds the core for a bare-metal target and builds and lints the firmware for all four chips (`.github/workflows/ci.yml`).

## Acknowledgements and license

Project layout and tooling are modelled on [shottimer](https://github.com/michidk/shottimer). Built on [esp-hal](https://github.com/esp-rs/esp-hal), [embassy](https://embassy.dev), [TrouBLE](https://github.com/embassy-rs/trouble) and [sequential-storage](https://github.com/tweedegolf/sequential-storage).

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
