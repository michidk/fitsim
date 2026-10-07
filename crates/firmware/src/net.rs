//! Wi-Fi bring-up and network services.
//!
//! Two modes, chosen at boot (switching = reboot):
//!
//! * **Station**: join the saved network, DHCP, answer mDNS for `<hostname>.local`.
//! * **Provisioning**: open access point `FitnessSimulator-XXXX` on 192.168.4.1 with a DHCP server
//!   and captive-portal DNS. Network scans run on the (idle) station interface.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{
    IpEndpoint, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::scan::{ScanConfig, ScanTypeConfig};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, ControllerConfig, Interface,
    WifiController,
};
use fitsim_core::event::{Kind, Level};
use fitsim_core::net::{self, AP_IP, AP_PREFIX_LEN, dhcp::DhcpServer, mdns};
use fitsim_core::settings::WifiConfig;
use portable_atomic::Ordering;
use static_cell::StaticCell;

use crate::shared::{Network, SCAN_REQUEST, SCAN_RESULT, ScanResult, Shared, WIFI_RSSI};

/// Sockets embassy-net can hold: HTTP workers + mDNS/DHCP/DNS + the DHCP client.
const NET_SOCKETS: usize = crate::http::WORKERS + 5;
/// Station failures (~5 s apart) before rebooting into provisioning mode.
const MAX_STA_FAILURES: u32 = 24;
/// Provisioning mode gives up (and retries the saved network) after this long without a client.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
pub enum Mode {
    Station(WifiConfig),
    /// `has_saved` = credentials exist, so the access point is only a temporary fallback.
    Provision {
        has_saved: bool,
    },
}

static RESOURCES: StaticCell<StackResources<NET_SOCKETS>> = StaticCell::new();

fn controller_config(initial: Config) -> ControllerConfig {
    // Smaller buffers than the defaults: this device serves a couple of browser tabs, not iperf.
    ControllerConfig::default()
        .with_initial_config(initial)
        .with_static_rx_buf_num(6)
        .with_dynamic_rx_buf_num(16)
        .with_dynamic_tx_buf_num(16)
}

/// Starts Wi-Fi in the requested mode and returns the network stack.
pub fn start(
    spawner: Spawner,
    wifi: WIFI<'static>,
    shared: Shared,
    mode: Mode,
    hostname: String,
    seed: u64,
) -> Stack<'static> {
    match mode {
        Mode::Station(creds) => {
            let auth = if creds.password.is_empty() {
                AuthenticationMethodConfig::Open
            } else {
                AuthenticationMethodConfig::WpaWpa2Personal(
                    creds
                        .password
                        .as_str()
                        .try_into()
                        .expect("password length validated"),
                )
            };
            let station = StationConfig::default()
                .with_ssid(
                    creds
                        .ssid
                        .as_str()
                        .try_into()
                        .expect("ssid length validated"),
                )
                .with_authentication(auth);
            let iface = Interface::station();
            let controller = WifiController::new(wifi, controller_config(Config::Station(station)))
                .expect("wifi init");

            let mut dhcp = embassy_net::DhcpConfig::default();
            dhcp.hostname = heapless::String::try_from(hostname.as_str()).ok();
            let (stack, runner) = embassy_net::new(
                iface,
                embassy_net::Config::dhcpv4(dhcp),
                RESOURCES.init(StackResources::new()),
                seed,
            );

            shared.with(|s| {
                s.platform.wifi.mode = "station";
                s.platform.wifi.ssid = creds.ssid.clone();
            });
            spawner.spawn(net_task(runner).unwrap());
            spawner.spawn(station_task(controller, shared).unwrap());
            spawner.spawn(mdns_task(stack, hostname).unwrap());
            stack
        }
        Mode::Provision { has_saved } => {
            let iface = Interface::access_point();
            let mac = iface.mac_address();
            let ssid = format!("FitnessSimulator-{:02X}{:02X}", mac[4], mac[5]);
            let ap = AccessPointConfig::default()
                .with_ssid(ssid.as_str().try_into().expect("ssid fits"));
            // The station half is only used for scanning networks while the AP is up.
            let controller = WifiController::new(
                wifi,
                controller_config(Config::AccessPointStation(StationConfig::default(), ap)),
            )
            .expect("wifi init");

            let gw = Ipv4Address::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3]);
            let cfg = embassy_net::Config::ipv4_static(StaticConfigV4 {
                address: Ipv4Cidr::new(gw, AP_PREFIX_LEN),
                gateway: Some(gw),
                dns_servers: Default::default(),
            });
            let (stack, runner) =
                embassy_net::new(iface, cfg, RESOURCES.init(StackResources::new()), seed);

            shared.with(|s| {
                s.platform.wifi.mode = "accessPoint";
                s.platform.wifi.ssid = ssid.clone();
                s.platform.wifi.ip = format!("{}.{}.{}.{}", AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3]);
                s.log(
                    Level::Info,
                    Kind::System,
                    format!("Provisioning mode: join '{ssid}' and open http://192.168.4.1"),
                );
            });
            spawner.spawn(net_task(runner).unwrap());
            spawner.spawn(access_point_task(controller, shared, has_saved).unwrap());
            spawner.spawn(dhcp_task(stack).unwrap());
            spawner.spawn(dns_task(stack).unwrap());
            stack
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

// ---- Wi-Fi scanning -----------------------------------------------------------------------------

async fn scan(controller: &mut WifiController<'static>) -> ScanResult {
    // Longer dwell than the 10/20 ms default: the access point keeps serving the phone's channel,
    // so short scans can come back empty.
    let config = ScanConfig::default()
        .with_max(24)
        .with_scan_type(ScanTypeConfig::Active {
            min: esp_hal::time::Duration::from_millis(40),
            max: esp_hal::time::Duration::from_millis(120),
        });
    let mut found = Vec::new();
    for attempt in 1..=2 {
        found = controller.scan_async(&config).await.map_err(|e| {
            log::warn!("wifi scan: {e:?}");
            "scan failed"
        })?;
        log::info!("wifi scan {attempt}: {} access points", found.len());
        if !found.is_empty() {
            break;
        }
        Timer::after(Duration::from_millis(300)).await;
    }
    let mut nets: Vec<Network> = Vec::new();
    for ap in found {
        let ssid = ap.ssid.as_str();
        if ssid.is_empty() {
            continue; // hidden network
        }
        let secure = ap.auth_method != Some(AuthenticationMethod::None);
        match nets.iter_mut().find(|n| n.ssid == ssid) {
            Some(existing) if existing.rssi >= ap.signal_strength => {}
            Some(existing) => {
                existing.rssi = ap.signal_strength;
                existing.secure = secure;
            }
            None => nets.push(Network {
                ssid: ssid.to_string(),
                rssi: ap.signal_strength,
                secure,
            }),
        }
    }
    nets.sort_by_key(|n| core::cmp::Reverse(n.rssi));
    Ok(nets)
}

// ---- Station mode -------------------------------------------------------------------------------

#[embassy_executor::task]
async fn station_task(mut controller: WifiController<'static>, shared: Shared) {
    let mut failures = 0u32;
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                failures = 0;
                log::info!("wifi: connected {info:?}");
                shared.with(|s| s.log(Level::Info, Kind::System, "Wi-Fi connected"));
                loop {
                    match select3(
                        controller.wait_for_disconnect_async(),
                        SCAN_REQUEST.wait(),
                        Timer::after(Duration::from_secs(5)),
                    )
                    .await
                    {
                        Either3::First(_) => break,
                        Either3::Second(()) => SCAN_RESULT.signal(scan(&mut controller).await),
                        Either3::Third(()) => {
                            let rssi = controller.rssi().ok().unwrap_or(i32::MIN);
                            WIFI_RSSI.store(rssi, Ordering::Relaxed);
                        }
                    }
                }
                WIFI_RSSI.store(i32::MIN, Ordering::Relaxed);
                shared.with(|s| s.log(Level::Warn, Kind::System, "Wi-Fi disconnected"));
            }
            Err(e) => {
                failures += 1;
                log::warn!("wifi: connect failed ({failures}/{MAX_STA_FAILURES}): {e:?}");
                if failures >= MAX_STA_FAILURES {
                    shared.with(|s| {
                        s.log(
                            Level::Error,
                            Kind::System,
                            "Wi-Fi unreachable, rebooting into setup mode",
                        )
                    });
                    crate::system::reboot_into_provisioning(shared).await;
                }
                // A scan request can't be served while not associated; answer instead of hanging.
                if SCAN_REQUEST.signaled() {
                    SCAN_REQUEST.reset();
                    SCAN_RESULT.signal(Err("not connected to a network"));
                }
            }
        }
        Timer::after(Duration::from_secs(5)).await;
    }
}

// ---- Provisioning mode --------------------------------------------------------------------------

#[embassy_executor::task]
async fn access_point_task(
    mut controller: WifiController<'static>,
    shared: Shared,
    has_saved: bool,
) {
    let mut deadline = Instant::now() + PROVISION_TIMEOUT;
    loop {
        let timeout = async {
            if has_saved {
                Timer::at(deadline).await
            } else {
                core::future::pending::<()>().await
            }
        };
        match select3(
            controller.wait_for_access_point_connected_event_async(),
            SCAN_REQUEST.wait(),
            timeout,
        )
        .await
        {
            Either3::First(ev) => {
                // Somebody is configuring the device: keep the access point up.
                deadline = Instant::now() + PROVISION_TIMEOUT;
                log::info!("wifi ap: {ev:?}");
            }
            Either3::Second(()) => SCAN_RESULT.signal(scan(&mut controller).await),
            Either3::Third(()) => {
                shared.with(|s| {
                    s.log(
                        Level::Info,
                        Kind::System,
                        "Setup timed out, retrying saved Wi-Fi",
                    )
                });
                crate::system::reboot().await;
            }
        }
    }
}

#[embassy_executor::task]
async fn dhcp_task(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx = [0u8; 600];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx = [0u8; 600];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    if socket.bind(67).is_err() {
        log::error!("dhcp: cannot bind port 67");
        return;
    }
    let mut server = DhcpServer::new(AP_IP);
    let mut buf = [0u8; 600];
    let broadcast = IpEndpoint::new(Ipv4Address::BROADCAST.into(), 68);
    loop {
        let Ok((n, _)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if let Some(reply) = server.handle(&buf[..n]) {
            let _ = socket.send_to(&reply, broadcast).await;
        }
    }
}

#[embassy_executor::task]
async fn dns_task(stack: Stack<'static>) {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx = [0u8; 512];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    if socket.bind(53).is_err() {
        log::error!("dns: cannot bind port 53");
        return;
    }
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if let Some(reply) = net::dns::respond(&buf[..n], AP_IP) {
            let _ = socket.send_to(&reply, meta).await;
        }
    }
}

// ---- mDNS (station mode) ------------------------------------------------------------------------

#[embassy_executor::task]
async fn mdns_task(stack: Stack<'static>, hostname: String) {
    stack.wait_config_up().await;
    let group = Ipv4Address::from(net::mdns::MULTICAST_ADDR);
    if let Err(e) = stack.join_multicast_group(group) {
        log::warn!("mdns: cannot join multicast group: {e:?}");
        return;
    }
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx = [0u8; 768];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx = [0u8; 768];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    if socket.bind(mdns::PORT).is_err() {
        log::warn!("mdns: cannot bind port {}", mdns::PORT);
        return;
    }
    let to_group = IpEndpoint::new(group.into(), mdns::PORT);
    let mut buf = [0u8; 768];
    let mut announced = None;
    loop {
        let Some(cfg) = stack.config_v4() else {
            Timer::after(Duration::from_secs(1)).await;
            continue;
        };
        let ip = cfg.address.address().octets();
        if announced != Some(ip) {
            announced = Some(ip);
            log::info!(
                "mdns: {hostname}.local -> {}.{}.{}.{}",
                ip[0],
                ip[1],
                ip[2],
                ip[3]
            );
            let _ = socket
                .send_to(&mdns::announcement(&hostname, ip), to_group)
                .await;
        }
        // Wake up regularly to notice DHCP address changes.
        match select(
            socket.recv_from(&mut buf),
            Timer::after(Duration::from_secs(30)),
        )
        .await
        {
            Either::First(Ok((n, meta))) => {
                if let Some(reply) = mdns::respond(&buf[..n], &hostname, ip) {
                    let dest = if reply.unicast {
                        meta.endpoint
                    } else {
                        to_group
                    };
                    let _ = socket.send_to(&reply.payload, dest).await;
                }
            }
            Either::First(Err(_)) | Either::Second(()) => {}
        }
    }
}
