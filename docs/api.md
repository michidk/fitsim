# HTTP and WebSocket API

Everything the web UI does goes through this API, so anything the UI can do a script can do too.
The same router runs on the ESP32 and in the desktop `fitsim-host`, so you can develop against
`http://localhost:8080` without hardware.

- Base URL: `http://fitness-simulator.local` (or the device IP).
- All bodies are JSON (`Content-Type` is not checked). Responses are JSON unless noted.
- Errors: `{"error": "message"}` with `400` (bad JSON), `404`, `405`, `409` (state conflict),
  `413`, `422` (valid JSON, invalid value).
- CORS is open (`Access-Control-Allow-Origin: *`); there is no authentication. Do not expose the
  device to untrusted networks.
- Field names are `camelCase`. The heart-rate metric slug is `heart-rate`.
- Times are device **uptime in milliseconds** (`t`, `timestamp`, `connectedMs`) unless stated.
  To show wall-clock times, `POST /api/time` once and add `time.wallOffsetMs` to an uptime to get
  Unix milliseconds.

Metrics: `speed` (km/h), `cadence` (rpm), `power` (W), `heart-rate` (bpm), `resistance` (%).

## State

| Method | Path | Description |
|---|---|---|
| GET | `/api/state` | Full snapshot (see below) |
| GET | `/api/telemetry` | Live values only |
| GET | `/api/system` | Version, chip, uptime, heap, Wi-Fi |
| POST | `/api/time` | `{"epochMs": 1700000000000}` sync the wall clock |

### `GET /api/state`

```json
{
  "time": { "uptimeMs": 26902, "wallOffsetMs": null },
  "telemetry": {
    "speed": 27.1, "cadence": 0.0, "power": 250.0, "heartRate": 80.0, "resistance": 20.0,
    "targetPower": 250, "targetResistance": null
  },
  "metrics": {
    "speed":      { "label": "Speed", "unit": "km/h", "min": 0, "max": 120,  "generator": { "mode": "manual", "value": 0 }, "source": "generator" },
    "cadence":    { "label": "Cadence", "unit": "rpm", "min": 0, "max": 200, "generator": { "mode": "manual", "value": 0 }, "source": "generator" },
    "power":      { "label": "Power", "unit": "W", "min": 0, "max": 2500,    "generator": { "mode": "manual", "value": 123.4 }, "source": "trainer" },
    "heartRate":  { "label": "Heart rate", "unit": "bpm", "min": 0, "max": 250, "generator": { "mode": "manual", "value": 80 }, "source": "generator" },
    "resistance": { "label": "Resistance", "unit": "%", "min": 0, "max": 100, "generator": { "mode": "manual", "value": 20 }, "source": "generator" }
  },
  "ble": { "advertising": true, "devices": { "trainer": { … }, "heartRate": { … }, "powerMeter": { … } }, "clients": [ … ] },
  "trainer": { … },
  "settings": { … },
  "system": { … }
}
```

`metrics.<m>.source` says what currently determines the value: `generator`, or `trainer` (an FTMS
power / resistance target). While the source is `trainer` the metric's own generator keeps
running but is overridden.

`telemetry.targetPower` / `targetResistance` are the **FTMS targets** requested by the client app.
The simulated trainer reaches them instantly, so `telemetry.power` equals the target while an ERG
target is active.

## Metrics and generators

| Method | Path | Body | Description |
|---|---|---|---|
| GET | `/api/state/{metric}` | | `{metric, value, unit, source, generator}` |
| POST/PUT | `/api/state/{metric}` | `{"value": 250}` | Manual mode with that value |
| GET | `/api/metrics/{metric}` | | The generator configuration |
| PUT/POST | `/api/metrics/{metric}` | generator (below) | Replace the generator, restarts its clock |

Generator bodies (`mode` selects the shape; all numbers are in the metric's unit):

```json
{ "mode": "fixed",  "value": 250 }
{ "mode": "manual", "value": 250 }
{ "mode": "ramp", "start": 100, "end": 400, "durationS": 60, "repeat": false }
{ "mode": "oscillation", "center": 150, "amplitude": 10, "periodS": 20, "waveform": "sine" }
{ "mode": "randomVariation", "base": 90, "variation": 5, "intervalS": 1 }
{ "mode": "randomWalk", "min": 130, "max": 170, "maxStepPerS": 2 }
{ "mode": "sequence", "points": [ {"t": 0, "v": 100}, {"t": 10, "v": 150} ], "interpolate": true, "repeat": false }
```

- `waveform`: `sine` | `triangle` | `square` | `saw`. All waves start at the minimum
  (`center - amplitude`), so the spec example reads 140 → 150 → 160 → 150 → 140.
- `ramp` holds the end value afterwards unless `repeat` is true.
- `sequence`: up to 64 points, `t` seconds non-decreasing; holds the last value (or loops).
- Values are clamped to the metric range (`min`/`max` in the snapshot).

## BLE

`GET /api/ble` returns `{advertising, devices: {trainer, heartRate, powerMeter}, clients: [...]}`.
A device is `connected` while a client is subscribed to one of its characteristics:

```json
{ "connected": true, "clients": 1, "connectedSinceMs": 12224, "mtu": 247, "rssi": -58,
  "subscribed": ["indoorBikeData", "controlPoint"] }
```

`clients[]`: `{id, peer, connectedMs, mtu, rssi, subscribed}`, where `connectedMs` is the uptime at
which the client connected. Characteristic names: `indoorBikeData`, `machineStatus`,
`controlPoint`, `trainingStatus`, `heartRateMeasurement`, `cyclingPowerMeasurement`.

## Trainer / FTMS

| Method | Path | Description |
|---|---|---|
| GET | `/api/trainer` | Trainer status |
| GET | `/api/ftms/control` | `{control: <trainer status>, recentCommands: [event…]}` – last 20 Control Point commands |
| GET | `/api/ftms/commands?since=<seq>` | `{commands: [event…], lastSeq}` |
| POST | `/api/ftms/reset` | Clear control owner, targets and simulation parameters |

Trainer status:

```json
{ "connected": true, "controlOwner": 64,
  "machineState": "started",
  "targetPower": 250, "targetResistance": null,
  "simulation": { "windSpeed": -1.2, "grade": 7.4, "crr": 0.004, "cw": 0.51 },
  "lastCommand": { "t": 12224, "command": "setTargetPower", "name": "Set Target Power",
                   "result": "success", "client": 64, "summary": "Set Target Power = 250 W" },
  "commandsReceived": 12 }
```

- `controlOwner`: BLE connection handle of the client that holds FTMS control, or `null`.
- `machineState`: `stopped` | `started` | `paused`.
- `simulation` is `null` until a client sends *Set Indoor Bike Simulation Parameters*
  (`windSpeed` m/s, `grade` %, `crr`, `cw` kg/m).
- `targetResistance` is a percentage (0–100).
- Command names (`command`): `requestControl`, `reset`, `start`, `stop`, `pause`,
  `setTargetPower`, `setTargetResistance`, `setIndoorBikeSimulation`, and for unsupported ones
  `setTargetSpeed`, `setTargetInclination`, `setTargetHeartRate`, `setWheelCircumference`,
  `spinDown`, `setTargetCadence`, `unknown`.
- `result`: `success`, `notSupported`, `invalidParameter`, `controlNotPermitted`, `operationFailed`.

## Settings, Wi-Fi, system

| Method | Path | Description |
|---|---|---|
| GET | `/api/settings` | Settings object |
| PUT/POST | `/api/settings` | Partial update, deep-merged; persisted to flash |
| GET | `/api/wifi` | `{mode, ssid, ip, rssi, hostname}` (never the password) |
| GET | `/api/wifi/scan` | `{networks: [{ssid, rssi, secure}]}` (device/host only; may take a few seconds) |
| POST | `/api/wifi` | `{"ssid": "…", "password": "…"}` save and reboot |
| POST | `/api/system/reboot` | Reboot |
| POST | `/api/system/factory-reset` | Erase settings and Wi-Fi, then reboot |

Settings:

```json
{ "hostname": "fitness-simulator", "deviceName": "DebugTrainer" }
```

`deviceName` is the advertised BLE name (1-24 printable ASCII bytes, applied immediately);
`hostname` is `a-z0-9-` (at most 32) and applies after a reboot.

`system`: `{version, chip, uptimeMs, heapFree, heapTotal, wifiMode, ssid, ip, rssi, hostname}`
with `wifiMode` `station` | `accessPoint` | `none`. In `accessPoint` mode the device is in
**provisioning mode**: the UI should show a Wi-Fi setup screen (scan, pick network, password,
`POST /api/wifi`).

## WebSocket `/ws`

Connect to `ws://<host>/ws`. Text frames, one JSON object each; every frame has `type` and
`timestamp` (uptime ms). The server pushes; client frames are ignored except ping/close.

| `type` | Payload | When |
|---|---|---|
| `hello` | `data: {version, protocol, lastEventSeq}` | first frame |
| `state` | `data:` the full `/api/state` snapshot | on connect and whenever non-telemetry state changed (generator, settings, BLE, trainer …) |
| `telemetry` | `data:` the `telemetry` object | every 200 ms |
| `log` | `data:` an event (see Event log) | every new log entry |
| `ftms-command` | `command`, `value?`, `message`, `data: {command,name,value?,unit?,result,client,raw}` | each Control Point write |
| `ble-state` | `data: ble` | BLE connect/disconnect/subscribe |
| `log-cleared` | | the log was cleared |

```json
{ "type": "telemetry", "timestamp": 13226,
  "data": { "speed": 6.6, "cadence": 0, "power": 121.8, "heartRate": 80, "resistance": 20,
            "targetPower": 250, "targetResistance": null } }
{ "type": "ftms-command", "timestamp": 12224, "command": "setTargetPower", "value": 250,
  "message": "Set Target Power = 250 W",
  "data": { "command": "setTargetPower", "name": "Set Target Power", "value": 250, "unit": "W",
            "result": "success", "client": 9, "raw": "05fa00" } }
```

Reconnect on close; on reconnect you get `hello` + `state` again. Use `lastEventSeq` from `hello`
with `GET /api/events?since=` to avoid gaps; de-duplicate by `seq`.

## Desktop-only debug endpoints (`fitsim-host`)

These stand in for a BLE client and do not exist on the firmware:

```
POST /api/debug/ble         {"event":"connect","client":64,"mtu":247}
                            {"event":"subscribe","client":64,"char":"indoorBikeData"}
                            {"event":"unsubscribe" | "disconnect" | "update", …, "rssi": -58}
POST /api/debug/ftms-write  {"client":64,"hex":"05b400"}   → {"response":"800501","status":"08b400","accepted":true}
```

Run `fitsim-host --provisioning` to simulate the setup access point (`system.wifiMode == "accessPoint"`).
