# Bluetooth LE profile reference

What the simulator exposes over the air. The byte layouts are implemented and unit-tested in
`crates/fitsim-core/src/{ftms,hrs,cps}.rs`; the GATT table is declared in
`crates/firmware/src/ble.rs`. UUIDs are the 16-bit Bluetooth SIG assigned numbers.

## Fitness Machine Service (FTMS 1.0), `0x1826`

| UUID | Characteristic | Properties | Value |
|---|---|---|---|
| `2ACC` | Fitness Machine Feature | Read | `82 44 00 00 0C 20 00 00` |
| `2AD2` | Indoor Bike Data | Notify | see below |
| `2AD3` | Training Status | Read, Notify | `[flags 0x00, status]`: Idle `01`, Watt Control `0C`, Manual Mode `0D` |
| `2AD6` | Supported Resistance Level Range | Read | min `0`, max `100`, step `1` (units of 0.1) |
| `2AD8` | Supported Power Range | Read | min `0` W, max `2000` W, step `1` W |
| `2AD9` | Fitness Machine Control Point | Write, Indicate | requests and `0x80` responses, below |
| `2ADA` | Fitness Machine Status | Notify | status code + parameters, below |

**Fitness Machine Feature**: Cadence, Resistance Level, Heart Rate Measurement, Power Measurement. **Target Setting Features**: Resistance Target, Power
Target, Indoor Bike Simulation Parameters.

**Advertisement**: the FTMS Service Data AD structure (`0x1826`, flags `01` = available, machine
type `0x0020` = indoor bike) is included, as the spec requires.

### Indoor Bike Data (`2AD2`)

Flags (`uint16`): bit 0 *More Data* is **clear** (instantaneous speed is present), then the
optional fields below in spec order. When the negotiated ATT MTU is small, optional fields are
dropped (heart rate first, then resistance) so every notification fits `MTU - 3` bytes. All fields
fit even at the default MTU of 23 (11 bytes).

| Field | Flag bit | Type | Resolution |
|---|---|---|---|
| Instantaneous Speed | (always) | `uint16` | 0.01 km/h |
| Instantaneous Cadence | 2 | `uint16` | 0.5 rpm |
| Resistance Level | 5 | `sint16` | 1 (0-100, same scale as the simulator's resistance %) |
| Instantaneous Power | 6 | `sint16` | 1 W |
| Heart Rate | 9 | `uint8` | 1 bpm |

Distance, elapsed time, energy, average values and remaining time are not sent. Values saturate
at the field limits instead of wrapping.

### Control Point (`2AD9`)

| Op code | Request | Parameters | Result |
|---|---|---|---|
| `00` | Request Control | none | success, or `Control Not Permitted` if another client owns it |
| `01` | Reset | none | success; status `01` |
| `04` | Set Target Resistance Level | `uint8` (0.1), `0..=100` | success; status `07` + value. Otherwise `Invalid Parameter` |
| `05` | Set Target Power | `sint16` W, `0..=2000` | success; status `08` + value. Otherwise `Invalid Parameter` |
| `07` | Start or Resume | none | success; status `04` |
| `08` | Stop or Pause | `01` stop / `02` pause | success; status `02` + parameter. Otherwise `Invalid Parameter` |
| `11` | Set Indoor Bike Simulation Parameters | wind `sint16` 0.001 m/s, grade `sint16` 0.01 %, Crr `uint8` 0.0001, Cw `uint8` 0.01 kg/m | success; status `12` + the six parameter bytes |
| `02` `03` `06` `09`-`10` `12` `13` `14` | everything else | | `Op Code Not Supported` |

Response indication: `[0x80, request op code, result]` with result `01` success, `02` op code not
supported, `03` invalid parameter, `04` operation failed, `05` control not permitted. A payload
of the wrong length for a known op code is `Invalid Parameter`.

Behaviour notes (choices the spec leaves open):

- Without a prior *Request Control* every op code except *Request Control* is `Control Not
  Permitted`.
- *Request Control* from the current owner succeeds again; from another client it is denied until
  the owner disconnects.
- *Reset* keeps control permission.
- A target resistance and a target power replace each other, and *Set Indoor Bike Simulation
  Parameters* replaces both (simulation mode). *Stop* clears targets; *Pause* keeps them.
- Responses go out as indications after the ATT write response. The result is only delivered if
  the client enabled indications on the Control Point; the command is applied either way.

### Fitness Machine Status (`2ADA`)

`01` Reset, `02` Stopped or Paused by user (`01` stop / `02` pause), `04` Started or Resumed by
user, `07` Target Resistance Level Changed (`uint8`), `08` Target Power Changed (`sint16`), `12`
Indoor Bike Simulation Parameters Changed (6 bytes). Sent to every client subscribed to the
characteristic.

## Heart Rate Service (HRS 1.0), `0x180D`

| UUID | Characteristic | Properties | Value |
|---|---|---|---|
| `2A37` | Heart Rate Measurement | Notify | flags, 8-bit heart rate, RR intervals |
| `2A38` | Body Sensor Location | Read | `1` = chest |

Flags: heart-rate format is always 8-bit; bit 2 (sensor contact supported) is set and bit 1
(contact detected) is set while the heart rate is above zero; bit 4 (RR-Interval present) is set
when beats completed since the previous notification. RR intervals are `uint16` in 1/1024 s,
generated from the simulated heart rate (up to four per notification). Energy expended is not
sent; the Heart Rate Control Point is not implemented.

## Cycling Power Service (CPS 1.1), `0x1818`

| UUID | Characteristic | Properties | Value |
|---|---|---|---|
| `2A63` | Cycling Power Measurement | Notify | 10 bytes, below |
| `2A65` | Cycling Power Feature | Read | `0x00000008`: crank revolution data |
| `2A5D` | Sensor Location | Read | `5` = left crank |

Measurement (8 bytes): `flags uint16 (0x0020)`, `instantaneous power sint16 (W)`, `cumulative crank
revolutions uint16`, `last crank event time uint16 (1/1024 s)`.
The crank data is integrated from the simulated cadence exactly like a real sensor: the event time
is the moment the last revolution completed, so a client recovers the cadence from two samples.
Counters wrap at 16 bits. Accumulated energy, pedal balance, torque, wheel revolutions, extreme magnitudes and the
Cycling Power Control Point are not implemented.

## Device Information Service, `0x180A`

Manufacturer Name (`2A29`, "Fitness Simulator"), Model Number (`2A24`, the chip name) and Firmware
Revision (`2A26`, the firmware version), all read-only.

## GAP

Device Name (the trainer's name, updated at boot) and Appearance (*Generic Cycling*). There is no
pairing, bonding or encryption; connections use LE legacy security level 1 (none).

## How the device is advertised

One legacy connectable, scannable advertisement carries the service UUIDs of the trainer, heart-rate
and power-meter services plus the FTMS service data; the device name (default `DebugTrainer`,
configurable) is in the scan response. All three services always live in the same GATT table, under
one address (a random static address derived from the chip MAC). Up to three clients can be
connected at once, and the device keeps advertising while slots are free. Changing the name in the
UI restarts advertising with the new name.

Notification rates are fixed: Indoor Bike Data and Cycling Power Measurement 4 Hz, Heart Rate
Measurement 1 Hz.
