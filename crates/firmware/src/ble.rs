//! Bluetooth LE peripheral: FTMS trainer, Heart Rate sensor and Cycling Power meter.
//!
//! One GATT server hosts all three services and one connectable advertisement announces them
//! together under a single name and address, so apps list one device that can fill every role.

use alloc::format;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::BT;
use esp_radio::ble::controller::BleConnector;
use fitsim_core::cps;
use fitsim_core::ftms::{self, TrainingStatus};
use fitsim_core::hrs;
use fitsim_core::metric::Device;
use fitsim_core::sim::{Char, notify_interval_ms};
use portable_atomic::{AtomicU8, AtomicU16, Ordering};
use static_cell::StaticCell;
use trouble_host::prelude::*;

use crate::shared::Shared;

const MAX_CONNECTIONS: usize = 3;
/// ATT + signalling channel per connection, plus headroom.
const L2CAP_CHANNELS: usize = 8;

// ---- GATT database ------------------------------------------------------------------------------

#[gatt_server(connections_max = 3)]
pub struct Server {
    ftms: FitnessMachineService,
    hrs: HeartRateService,
    cps: CyclingPowerService,
    dis: DeviceInformationService,
}

/// Fitness Machine Service (0x1826), indoor bike.
#[gatt_service(uuid = "1826")]
struct FitnessMachineService {
    #[characteristic(uuid = "2acc", read, value = ftms::feature_bytes())]
    feature: [u8; 8],
    #[characteristic(uuid = "2ad2", notify)]
    indoor_bike_data: [u8; 10],
    #[characteristic(uuid = "2ad3", read, notify, value = ftms::training_status(TrainingStatus::Idle))]
    training_status: [u8; 2],
    #[characteristic(uuid = "2ad6", read, value = ftms::supported_resistance_range())]
    supported_resistance_range: [u8; 6],
    #[characteristic(uuid = "2ad8", read, value = ftms::supported_power_range())]
    supported_power_range: [u8; 6],
    #[characteristic(uuid = "2ad9", write, indicate)]
    control_point: [u8; 20],
    #[characteristic(uuid = "2ada", notify)]
    machine_status: [u8; 12],
}

/// Heart Rate Service (0x180D).
#[gatt_service(uuid = "180d")]
struct HeartRateService {
    #[characteristic(uuid = "2a37", notify)]
    measurement: [u8; 10],
    #[characteristic(uuid = "2a38", read, value = 1)]
    body_sensor_location: u8,
}

/// Cycling Power Service (0x1818).
#[gatt_service(uuid = "1818")]
struct CyclingPowerService {
    #[characteristic(uuid = "2a63", notify)]
    measurement: [u8; 8],
    #[characteristic(uuid = "2a65", read, value = cps::feature_bytes())]
    feature: [u8; 4],
    #[characteristic(uuid = "2a5d", read, value = cps::SENSOR_LOCATION_LEFT_CRANK)]
    sensor_location: u8,
}

/// Device Information Service (0x180A).
#[gatt_service(uuid = "180a")]
struct DeviceInformationService {
    #[characteristic(uuid = "2a29", read, value = HeaplessString::try_from("Fitness Simulator").unwrap())]
    manufacturer_name: HeaplessString<24>,
    #[characteristic(uuid = "2a24", read, value = HeaplessString::try_from("ESP32 BLE Test Bench").unwrap())]
    model_number: HeaplessString<24>,
    #[characteristic(uuid = "2a26", read, value = HeaplessString::try_from(fitsim_core::VERSION).unwrap())]
    firmware_revision: HeaplessString<24>,
}

type Ctrl = ExternalController<BleConnector<'static>, 20>;
type Resources = HostResources<DefaultPacketPool, MAX_CONNECTIONS, L2CAP_CHANNELS>;
type BleStack = Stack<'static, Ctrl, DefaultPacketPool>;
type Conn = GattConnection<'static, 'static, DefaultPacketPool>;
type Periph = Peripheral<'static, Ctrl, DefaultPacketPool>;

static RESOURCES: StaticCell<Resources> = StaticCell::new();
static STACK: StaticCell<BleStack> = StaticCell::new();
static SERVER: StaticCell<Server<'static>> = StaticCell::new();

/// Currently connected clients (advertising stops when all slots are taken).
static ACTIVE: AtomicU8 = AtomicU8::new(0);
static NEXT_ID: AtomicU16 = AtomicU16::new(1);

/// Starts the controller, GATT server and background tasks.
pub fn start(spawner: Spawner, bt: BT<'static>, shared: Shared, mac: [u8; 6]) {
    let connector = BleConnector::new(bt, Default::default()).expect("ble controller init");
    let controller: Ctrl = ExternalController::new(connector);
    let resources = RESOURCES.init(HostResources::new());
    let stack = STACK.init(
        trouble_host::new(controller, resources)
            .set_random_address(Address::random(static_address(mac)))
            .build(),
    );

    let name = shared.with(|s| s.settings.device_name.clone());
    // The GAP "Device Name" characteristic needs a 'static string; it refreshes on reboot.
    let name: &'static str = alloc::boxed::Box::leak(name.into_boxed_str());
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name,
        appearance: &appearance::cycling::GENERIC_CYCLING,
    }))
    .expect("gatt server");
    // Device Information: model number names the chip this firmware runs on.
    if let Ok(model) = HeaplessString::<24>::try_from(crate::system::CHIP) {
        let _ = server.set(&server.dis.model_number, &model);
    }
    let server: &'static Server<'static> = SERVER.init(server);

    let runner = stack.runner();
    let peripheral = stack.peripheral();
    spawner.spawn(runner_task(runner).unwrap());
    spawner.spawn(advertising_task(peripheral, stack, server, shared, spawner).unwrap());
}

#[embassy_executor::task]
async fn runner_task(mut runner: Runner<'static, Ctrl, DefaultPacketPool>) {
    loop {
        if let Err(e) = runner.run().await {
            log::error!("ble: host runner error: {e:?}");
            Timer::after(Duration::from_millis(500)).await;
        }
    }
}

/// Random static address derived from the chip MAC (top two bits set).
fn static_address(mac: [u8; 6]) -> [u8; 6] {
    let mut a = mac;
    a[0] = a[0].wrapping_add(0x5A);
    a[5] |= 0xC0;
    a
}

type Adv = Advertiser<'static, Ctrl, DefaultPacketPool>;

/// One legacy connectable, scannable advertisement announces all three services under the
/// trainer's name; the name goes into the scan response.
async fn advertise(p: &mut Periph, name: &str) -> Result<Adv, alloc::string::String> {
    let uuids = [
        ftms::SERVICE_UUID.to_le_bytes(),
        hrs::SERVICE_UUID.to_le_bytes(),
        cps::SERVICE_UUID.to_le_bytes(),
    ];
    let svc_data = ftms::service_data();
    let (mut adv, mut scan) = ([0u8; 31], [0u8; 31]);
    let a = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids16(&uuids),
            AdStructure::ServiceData16 {
                uuid: ftms::SERVICE_UUID.to_le_bytes(),
                data: &svc_data,
            },
        ],
        &mut adv,
    )
    .map_err(|e| format!("{e:?}"))?;
    let s = AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(name.as_bytes())],
        &mut scan,
    )
    .map_err(|e| format!("{e:?}"))?;
    p.advertise(
        &AdvertisementParameters::default(),
        Advertisement::ConnectableScannableUndirected {
            adv_data: &adv[..a],
            scan_data: &scan[..s],
        },
    )
    .await
    .map_err(|e| format!("{e:?}"))
}

#[embassy_executor::task]
async fn advertising_task(
    mut p: Periph,
    stack: &'static BleStack,
    server: &'static Server<'static>,
    shared: Shared,
    spawner: Spawner,
) {
    loop {
        let (version, name) = shared.with(|s| (s.name_version(), s.settings.device_name.clone()));

        if ACTIVE.load(Ordering::Acquire) as usize >= MAX_CONNECTIONS {
            shared.with(|s| s.set_advertising(false));
            Timer::after(Duration::from_millis(500)).await;
            continue;
        }
        let advertiser = match advertise(&mut p, &name).await {
            Ok(a) => a,
            Err(e) => {
                log::warn!("ble: advertising failed: {e}");
                shared.with(|s| s.set_advertising(false));
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        };
        shared.with(|s| s.set_advertising(true));

        // Stop waiting when the name changes so the new name is advertised.
        let renamed = async {
            while shared.with(|s| s.name_version()) == version {
                Timer::after(Duration::from_millis(200)).await;
            }
        };
        match select(advertiser.accept(), renamed).await {
            Either::First(Ok(conn)) => match conn.with_attribute_server(server) {
                Ok(conn) => {
                    ACTIVE.fetch_add(1, Ordering::AcqRel);
                    spawner.spawn(connection_task(conn, server, stack, shared).unwrap());
                }
                Err(e) => log::warn!("ble: attribute server attach failed: {e:?}"),
            },
            Either::First(Err(e)) => {
                log::warn!("ble: accept failed: {e:?}");
                Timer::after(Duration::from_millis(500)).await;
            }
            Either::Second(()) => {}
        }
    }
}

// ---- Connections --------------------------------------------------------------------------------

#[embassy_executor::task(pool_size = 3)]
async fn connection_task(
    conn: Conn,
    server: &'static Server<'static>,
    stack: &'static BleStack,
    shared: Shared,
) {
    let id = NEXT_ID.fetch_add(1, Ordering::AcqRel);
    let peer = format!("{}", conn.raw().peer_address());
    shared.with(|s| s.client_connected(id, peer, conn.raw().att_mtu()));

    let outbox: Channel<NoopRawMutex, [u8; 3], 4> = Channel::new();
    select(
        gatt_events(&conn, server, shared, id, &outbox),
        notify_loop(&conn, server, stack, shared, id, &outbox),
    )
    .await;

    shared.with(|s| s.client_disconnected(id));
    ACTIVE.fetch_sub(1, Ordering::AcqRel);
}

/// Serves reads/writes. Control Point writes are handed to the simulator; the indication with the
/// result is queued for the notify loop so this loop never blocks on the peer's confirmation.
async fn gatt_events(
    conn: &Conn,
    server: &Server<'_>,
    shared: Shared,
    id: u16,
    outbox: &Channel<NoopRawMutex, [u8; 3], 4>,
) {
    let control_point = server.ftms.control_point.handle;
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                log::info!("ble: client {id} disconnected: {reason:?}");
                return;
            }
            GattConnectionEvent::Gatt { event } => {
                let reply = match event {
                    GattEvent::Write(w) if w.handle() == control_point => {
                        let mut data = [0u8; 20];
                        let n = w.with_data(|_, d| {
                            let n = d.len().min(data.len());
                            data[..n].copy_from_slice(&d[..n]);
                            n
                        });
                        let outcome = shared.with(|s| s.ftms_control_write(id, &data[..n]));
                        let _ = outbox.try_send(outcome.response);
                        w.accept_unprocessed()
                    }
                    GattEvent::Write(w) => w.accept(),
                    GattEvent::Read(r) => r.accept(),
                    other => other.accept(),
                };
                match reply {
                    Ok(r) => r.send().await,
                    Err(e) => log::warn!("ble: reply failed: {e:?}"),
                }
            }
            _ => {}
        }
    }
}

/// Pushes notifications at the configured rates, forwards FTMS status/indications and keeps the
/// simulator's view of this client (subscriptions, MTU, RSSI) up to date.
async fn notify_loop(
    conn: &Conn,
    server: &Server<'_>,
    stack: &BleStack,
    shared: Shared,
    id: u16,
    outbox: &Channel<NoopRawMutex, [u8; 3], 4>,
) {
    let ftms = &server.ftms;
    let mut next = [Instant::now(); 3];
    let mut subs = [false; 6];
    let mut status_seq = shared.with(|s| s.machine_status_seq());
    let mut last_training = TrainingStatus::Idle;
    let mut last_rssi = Instant::now();

    loop {
        // 0. The web UI asked to drop this client.
        if shared.with(|s| s.take_disconnect(id)) {
            conn.raw().disconnect();
            return;
        }

        // 1. Track subscriptions (CCCD state).
        let now_subs = [
            ftms.indoor_bike_data.should_notify(conn),
            ftms.machine_status.should_notify(conn),
            ftms.control_point.should_indicate(conn),
            ftms.training_status.should_notify(conn),
            server.hrs.measurement.should_notify(conn),
            server.cps.measurement.should_notify(conn),
        ];
        for (i, ch) in Char::ALL.iter().enumerate() {
            if now_subs[i] != subs[i] {
                shared.with(|s| s.client_subscription(id, *ch, now_subs[i]));
            }
        }
        subs = now_subs;
        let (bike, status, cp, training, hr, power) =
            (subs[0], subs[1], subs[2], subs[3], subs[4], subs[5]);

        // 2. Control Point indications (responses).
        while let Ok(resp) = outbox.try_receive() {
            if cp
                && ftms
                    .control_point
                    .indicate_raw(conn, &resp, false)
                    .await
                    .is_err()
            {
                return;
            }
        }

        // 3. Fitness Machine Status + Training Status.
        let (pending, training_now) = shared.with(|s| {
            (
                s.machine_status_since(status_seq),
                s.trainer.training_status(),
            )
        });
        for (seq, msg) in pending {
            status_seq = seq;
            if status
                && ftms
                    .machine_status
                    .notify_raw(conn, &msg, false)
                    .await
                    .is_err()
            {
                return;
            }
        }
        if training_now != last_training {
            last_training = training_now;
            if training
                && ftms
                    .training_status
                    .notify_raw(conn, &ftms::training_status(training_now), false)
                    .await
                    .is_err()
            {
                return;
            }
        }

        // 4. Periodic measurements.
        let now = Instant::now();
        for d in Device::ALL {
            if now < next[d.index()] {
                continue;
            }
            next[d.index()] = now + Duration::from_millis(notify_interval_ms(d) as u64);
            let ok = match d {
                Device::Trainer if bike => {
                    let mtu = conn.raw().att_mtu();
                    let p = shared.with(|s| s.bike_data_packet(mtu));
                    ftms.indoor_bike_data
                        .notify_raw(conn, p.as_slice(), false)
                        .await
                        .is_ok()
                }
                Device::HeartRate if hr => {
                    let p = shared.with(|s| s.heart_rate_packet());
                    server
                        .hrs
                        .measurement
                        .notify_raw(conn, p.as_slice(), false)
                        .await
                        .is_ok()
                }
                Device::PowerMeter if power => {
                    let p = shared.with(|s| s.power_packet());
                    server
                        .cps
                        .measurement
                        .notify_raw(conn, &p, false)
                        .await
                        .is_ok()
                }
                _ => true,
            };
            if !ok {
                return;
            }
        }

        // 5. Link statistics every few seconds.
        if now.duration_since(last_rssi) >= Duration::from_secs(3) {
            last_rssi = now;
            let rssi = conn.raw().rssi(stack).await.ok();
            let mtu = conn.raw().att_mtu();
            shared.with(|s| s.client_update(id, Some(mtu), rssi));
        }

        select(
            outbox.ready_to_receive(),
            Timer::after(Duration::from_millis(20)),
        )
        .await;
    }
}
