//! REST API routing.
//!
//! [`handle`] is a pure function from request to response plus [`Effect`]s (persist, reboot, …).
//! The firmware and the host dev server both call it, so the API behaves identically everywhere
//! and is covered by the unit tests below without any sockets.

use alloc::string::ToString;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::event::{Event, Kind, Level};
use crate::generator::Generator;
use crate::http::{Method, Response, query_param};
use crate::metric::Metric;
use crate::settings::{Settings, WifiConfig, merge_json};
use crate::sim::{Simulator, Source};

/// Side effects the platform layer has to perform *after* the response was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    PersistSettings,
    SaveWifi(WifiConfig),
    /// Erase settings and Wi-Fi credentials.
    FactoryReset,
    Reboot,
}

pub struct Request<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: &'a str,
    pub body: &'a [u8],
}

pub struct Output {
    pub response: Response,
    pub effects: Vec<Effect>,
}

impl Output {
    pub fn plain(response: Response) -> Self {
        Self {
            response,
            effects: Vec::new(),
        }
    }

    fn with(response: Response, effects: impl IntoIterator<Item = Effect>) -> Self {
        Self {
            response,
            effects: effects.into_iter().collect(),
        }
    }
}

type ApiResult = Result<Output, Response>;

fn bad(msg: impl AsRef<str>) -> Response {
    Response::error(400, msg.as_ref())
}

fn unprocessable(msg: impl AsRef<str>) -> Response {
    Response::error(422, msg.as_ref())
}

fn ok_json() -> Response {
    Response::json(200, &serde_json::json!({ "ok": true }))
}

fn parse<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, Response> {
    serde_json::from_slice(body).map_err(|e| bad(alloc::format!("invalid JSON body: {e}")))
}

fn metric_from(slug: &str) -> Result<Metric, Response> {
    Metric::from_slug(slug).ok_or_else(|| {
        Response::error(
            404,
            "unknown metric (speed, cadence, power, heart-rate, resistance)",
        )
    })
}

// Responses containing floats use typed structs: `serde_json::json!` converts f32 to f64 first,
// which would print `134.2` as `134.1999969482422`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricResponse<'a> {
    metric: Metric,
    value: f32,
    unit: &'static str,
    source: Source,
    generator: &'a Generator,
}

#[derive(Serialize)]
struct ValueSet {
    metric: Metric,
    value: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlResponse<'a> {
    control: crate::sim::TrainerStatus,
    recent_commands: Vec<&'a Event>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EventsResponse<'a> {
    events: Vec<&'a Event>,
    last_seq: u32,
    epoch: u32,
}

/// Entry point for everything under `/api`.
pub fn handle(sim: &mut Simulator, req: &Request<'_>) -> Output {
    let path = req.path.trim_matches('/');
    let segs: Vec<&str> = path.split('/').collect();
    let Some((&"api", rest)) = segs.split_first() else {
        return Output::plain(Response::error(404, "not found"));
    };
    match route(sim, req, rest) {
        Ok(out) => out,
        Err(resp) => Output::plain(resp),
    }
}

fn route(sim: &mut Simulator, req: &Request<'_>, segs: &[&str]) -> ApiResult {
    use Method::*;
    match (req.method, segs) {
        // ---- state ----------------------------------------------------------------------------
        (Get, ["state"]) => Ok(Output::plain(Response::json(200, &sim.snapshot()))),
        (Get, ["telemetry"]) => Ok(Output::plain(Response::json(200, &sim.telemetry()))),
        (Get, ["system"]) => Ok(Output::plain(Response::json(200, &sim.system_status()))),
        (Post | Put, ["time"]) => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Body {
                epoch_ms: i64,
            }
            let b: Body = parse(req.body)?;
            sim.set_wall_clock(b.epoch_ms);
            Ok(Output::plain(ok_json()))
        }

        // ---- metrics --------------------------------------------------------------------------
        (Get, ["state", slug]) => {
            let m = metric_from(slug)?;
            let snap = sim.snapshot();
            let info = match m {
                Metric::Speed => &snap.metrics.speed,
                Metric::Cadence => &snap.metrics.cadence,
                Metric::Power => &snap.metrics.power,
                Metric::HeartRate => &snap.metrics.heart_rate,
                Metric::Resistance => &snap.metrics.resistance,
            };
            Ok(Output::plain(Response::json(
                200,
                &MetricResponse {
                    metric: m,
                    value: crate::math::round_to(sim.value(m), 1),
                    unit: m.unit(),
                    source: info.source,
                    generator: info.generator,
                },
            )))
        }
        (Post | Put, ["state", slug]) => {
            #[derive(Deserialize)]
            struct Body {
                value: f32,
            }
            let m = metric_from(slug)?;
            let b: Body = parse(req.body)?;
            sim.set_value(m, b.value).map_err(unprocessable)?;
            Ok(Output::plain(Response::json(
                200,
                &ValueSet {
                    metric: m,
                    value: b.value,
                },
            )))
        }
        (Get, ["metrics", slug]) => {
            let m = metric_from(slug)?;
            Ok(Output::plain(Response::json(200, sim.generator(m))))
        }
        (Put | Post, ["metrics", slug]) => {
            let m = metric_from(slug)?;
            let g: Generator = parse(req.body)?;
            sim.set_generator(m, g).map_err(unprocessable)?;
            Ok(Output::plain(Response::json(200, sim.generator(m))))
        }

        // ---- BLE / trainer --------------------------------------------------------------------
        (Get, ["ble"]) => Ok(Output::plain(Response::json(200, &sim.ble_status()))),
        (Get, ["trainer"]) => Ok(Output::plain(Response::json(200, &sim.trainer_status()))),
        (Get, ["ftms", "control"]) => {
            let all: Vec<&Event> = sim.log.iter().filter(|e| e.ftms.is_some()).collect();
            let recent = all[all.len().saturating_sub(20)..].to_vec();
            Ok(Output::plain(Response::json(
                200,
                &ControlResponse {
                    control: sim.trainer_status(),
                    recent_commands: recent,
                },
            )))
        }
        (Post, ["ftms", "reset"]) => {
            sim.reset_trainer();
            Ok(Output::plain(ok_json()))
        }

        // ---- event log ------------------------------------------------------------------------
        (Get, ["events"]) => {
            let since = query_param(req.query, "since")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0u32);
            let limit = query_param(req.query, "limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100usize)
                .clamp(1, 200);
            let kind = query_param(req.query, "kind");
            let mut events: Vec<&Event> = sim
                .log
                .since(since)
                .filter(|e| {
                    kind.is_none_or(|k| {
                        serde_json::to_value(e.kind).is_ok_and(|v| v.as_str() == Some(k))
                    })
                })
                .collect();
            events.drain(..events.len().saturating_sub(limit));
            Ok(Output::plain(Response::json(
                200,
                &EventsResponse {
                    events,
                    last_seq: sim.log.last_seq(),
                    epoch: sim.log.epoch(),
                },
            )))
        }
        (Delete, ["events"]) | (Post, ["events", "clear"]) => {
            sim.log.clear();
            sim.bump();
            Ok(Output::plain(ok_json()))
        }
        (Get, ["events.txt"]) => {
            let mut r = Response::text(200, sim.log.to_text(sim.wall_offset_ms));
            r.content_disposition = Some("attachment; filename=\"fitness-simulator-events.txt\"");
            Ok(Output::plain(r))
        }

        // ---- settings, Wi-Fi, system ----------------------------------------------------------
        (Get, ["settings"]) => Ok(Output::plain(Response::json(200, &sim.settings))),
        (Put | Post, ["settings"]) => apply_settings_patch(sim, json_object(req.body)?),
        (Get, ["wifi"]) => {
            let s = sim.system_status();
            Ok(Output::plain(Response::json(
                200,
                &serde_json::json!({ "mode": s.wifi_mode, "ssid": s.ssid, "ip": s.ip, "rssi": s.rssi, "hostname": s.hostname }),
            )))
        }
        (Post | Put, ["wifi"]) => {
            let cfg: WifiConfig = parse(req.body)?;
            cfg.validate().map_err(unprocessable)?;
            sim.log(
                Level::Info,
                Kind::System,
                alloc::format!("Wi-Fi credentials saved for '{}', rebooting", cfg.ssid),
            );
            Ok(Output::with(
                Response::json(
                    200,
                    &serde_json::json!({ "ok": true, "message": "Wi-Fi saved, rebooting" }),
                ),
                [Effect::SaveWifi(cfg), Effect::Reboot],
            ))
        }
        (Post, ["system", "reboot"]) => {
            sim.log(Level::Warn, Kind::System, "Reboot requested");
            Ok(Output::with(
                Response::json(
                    200,
                    &serde_json::json!({ "ok": true, "message": "rebooting" }),
                ),
                [Effect::Reboot],
            ))
        }
        (Post, ["system", "factory-reset"]) => {
            sim.log(Level::Warn, Kind::System, "Factory reset requested");
            Ok(Output::with(
                Response::json(
                    200,
                    &serde_json::json!({ "ok": true, "message": "erasing settings, rebooting" }),
                ),
                [Effect::FactoryReset, Effect::Reboot],
            ))
        }

        // ---- fallbacks ------------------------------------------------------------------------
        (Options, _) => Ok(Output::plain(crate::http::preflight())),
        (_, [first, ..]) if KNOWN_ROOTS.contains(first) => {
            Err(Response::error(405, "method not allowed"))
        }
        _ => Err(Response::error(404, "not found")),
    }
}

const KNOWN_ROOTS: &[&str] = &[
    "state",
    "telemetry",
    "system",
    "time",
    "metrics",
    "ble",
    "settings",
    "trainer",
    "ftms",
    "events",
    "events.txt",
    "wifi",
];

fn json_object(body: &[u8]) -> Result<serde_json::Value, Response> {
    let v: serde_json::Value = parse(body)?;
    if v.is_object() {
        Ok(v)
    } else {
        Err(bad("body must be a JSON object"))
    }
}

fn apply_settings_patch(sim: &mut Simulator, patch: serde_json::Value) -> ApiResult {
    let mut merged =
        serde_json::to_value(&sim.settings).map_err(|e| Response::error(500, &e.to_string()))?;
    merge_json(&mut merged, patch);
    let new: Settings = serde_json::from_value(merged)
        .map_err(|e| unprocessable(alloc::format!("invalid settings: {e}")))?;
    sim.apply_settings(new).map_err(unprocessable)?;
    Ok(Output::with(
        Response::json(200, &sim.settings),
        [Effect::PersistSettings],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn sim() -> Simulator {
        Simulator::new(Settings::default(), 7)
    }

    fn call(
        sim: &mut Simulator,
        method: Method,
        path: &str,
        body: &str,
    ) -> (u16, serde_json::Value, Vec<Effect>) {
        let (p, q) = path.split_once('?').unwrap_or((path, ""));
        let out = handle(
            sim,
            &Request {
                method,
                path: p,
                query: q,
                body: body.as_bytes(),
            },
        );
        let json =
            serde_json::from_slice(out.response.body.as_slice()).unwrap_or(serde_json::Value::Null);
        (out.response.status, json, out.effects)
    }

    fn get(sim: &mut Simulator, path: &str) -> (u16, serde_json::Value) {
        let (s, j, _) = call(sim, Method::Get, path, "");
        (s, j)
    }

    fn post(sim: &mut Simulator, path: &str, body: &str) -> (u16, serde_json::Value) {
        let (s, j, _) = call(sim, Method::Post, path, body);
        (s, j)
    }

    #[test]
    fn spec_examples_work_verbatim() {
        let mut s = sim();
        assert_eq!(
            post(&mut s, "/api/state/power", r#"{ "value": 250 }"#).0,
            200
        );
        assert_eq!(
            post(&mut s, "/api/state/heart-rate", r#"{ "value": 155 }"#).0,
            200
        );
        s.tick(100);
        let (st, state) = get(&mut s, "/api/state");
        assert_eq!(st, 200);
        assert_eq!(state["telemetry"]["power"], 250.0);
        assert_eq!(state["telemetry"]["heartRate"], 155.0);
        let (st, ftms) = get(&mut s, "/api/ftms/control");
        assert_eq!(st, 200);
        assert!(ftms["control"]["controlOwner"].is_null());
        assert_eq!(ftms["recentCommands"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn generator_modes_via_metrics_endpoint() {
        let mut s = sim();
        let (st, _, _) = call(
            &mut s,
            Method::Put,
            "/api/metrics/power",
            r#"{"mode":"ramp","start":100,"end":400,"durationS":60}"#,
        );
        assert_eq!(st, 200);
        assert_eq!(get(&mut s, "/api/metrics/power").1["mode"], "ramp");
        let (st, j, _) = call(
            &mut s,
            Method::Put,
            "/api/metrics/power",
            r#"{"mode":"oscillation","center":1,"amplitude":1,"periodS":0}"#,
        );
        assert_eq!(st, 422, "{j}");
        let put = |s: &mut Simulator, path: &str, body: &str| call(s, Method::Put, path, body).0;
        assert_eq!(
            put(&mut s, "/api/metrics/power", r#"{"mode":"bogus"}"#),
            400,
            "unknown generator mode"
        );
        assert_eq!(
            put(
                &mut s,
                "/api/metrics/bogus",
                r#"{"mode":"manual","value":1}"#
            ),
            404
        );
        assert_eq!(
            post(&mut s, "/api/state/power", r#"{"value":99999}"#).0,
            422
        );
        assert_eq!(post(&mut s, "/api/state/power", r#"{"val":1}"#).0, 400);
    }

    #[test]
    fn settings_merge_and_effects() {
        let mut s = sim();
        let (st, j, fx) = call(
            &mut s,
            Method::Put,
            "/api/settings",
            r#"{"deviceName":"Kickr"}"#,
        );
        assert_eq!((st, fx), (200, alloc::vec![Effect::PersistSettings]));
        assert_eq!(j["deviceName"], "Kickr");
        assert_eq!(j["hostname"], "fitness-simulator");
        assert_eq!(s.settings.device_name, "Kickr");

        let put = |s: &mut Simulator, body: &str| call(s, Method::Put, "/api/settings", body).0;
        assert_eq!(put(&mut s, r#"{"hostname":"Not Valid"}"#), 422);
        assert_eq!(put(&mut s, r#"{"deviceName":5}"#), 422);
        assert_eq!(put(&mut s, "[1]"), 400);

        assert_eq!(call(&mut s, Method::Put, "/api/trainer", "{}").0, 405);
    }

    #[test]
    fn events_listing_filter_clear_and_text() {
        let mut s = sim();
        s.tick(1000);
        s.ftms_control_write(1, &[0x00]);
        s.ftms_control_write(1, &[0x05, 180, 0]);
        s.client_connected(1, "AA:BB".into(), 23);
        let (_, all) = get(&mut s, "/api/events");
        assert_eq!(all["events"].as_array().unwrap().len(), 4);
        let (_, f) = get(&mut s, "/api/events?kind=ftms");
        assert_eq!(f["events"].as_array().unwrap().len(), 3);
        let last = all["lastSeq"].as_u64().unwrap();
        let (_, none) = get(&mut s, &alloc::format!("/api/events?since={last}"));
        assert!(none["events"].as_array().unwrap().is_empty());
        let (_, limited) = get(&mut s, "/api/events?limit=2");
        assert_eq!(limited["events"].as_array().unwrap().len(), 2);

        let (_, ctl) = get(&mut s, "/api/ftms/control");
        assert_eq!(ctl["control"]["controlOwner"], 1);
        assert_eq!(ctl["control"]["targetPower"], 180);
        assert_eq!(ctl["control"]["lastCommand"]["command"], "setTargetPower");

        let out = handle(
            &mut s,
            &Request {
                method: Method::Get,
                path: "/api/events.txt",
                query: "",
                body: b"",
            },
        );
        assert_eq!(out.response.content_type, "text/plain; charset=utf-8");
        let text = String::from_utf8(out.response.body.as_slice().to_vec()).unwrap();
        assert!(text.contains("Set Target Power = 180 W"), "{text}");

        assert_eq!(call(&mut s, Method::Delete, "/api/events", "").0, 200);
        assert!(
            get(&mut s, "/api/events").1["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn system_and_wifi_effects() {
        let mut s = sim();
        let (st, j, fx) = call(
            &mut s,
            Method::Post,
            "/api/wifi",
            r#"{"ssid":"home","password":"hunter22"}"#,
        );
        assert_eq!(st, 200, "{j}");
        assert_eq!(
            fx,
            [
                Effect::SaveWifi(WifiConfig {
                    ssid: "home".into(),
                    password: "hunter22".into()
                }),
                Effect::Reboot
            ]
        );
        assert_eq!(
            call(
                &mut s,
                Method::Post,
                "/api/wifi",
                r#"{"ssid":"","password":""}"#
            )
            .0,
            422
        );
        assert_eq!(
            call(&mut s, Method::Post, "/api/system/reboot", "").2,
            [Effect::Reboot]
        );
        assert_eq!(
            call(&mut s, Method::Post, "/api/system/factory-reset", "").2,
            [Effect::FactoryReset, Effect::Reboot]
        );
        assert_eq!(
            get(&mut s, "/api/system").1["hostname"],
            "fitness-simulator"
        );
        // The Wi-Fi password is never readable.
        assert!(get(&mut s, "/api/wifi").1.get("password").is_none());
        assert_eq!(
            post(&mut s, "/api/time", r#"{"epochMs":1700000000000}"#).0,
            200
        );
        assert!(s.wall_offset_ms.is_some());
    }

    #[test]
    fn routing_errors() {
        let mut s = sim();
        assert_eq!(get(&mut s, "/api/nothing").0, 404);
        assert_eq!(get(&mut s, "/other").0, 404);
        assert_eq!(call(&mut s, Method::Delete, "/api/state", "").0, 405);
        assert_eq!(call(&mut s, Method::Options, "/api/state", "").0, 204);
        assert_eq!(
            get(&mut s, "/api/state/").0,
            200,
            "trailing slashes are tolerated"
        );
        assert_eq!(get(&mut s, "/api/state/unobtainium").0, 404);
    }

    #[test]
    fn full_state_is_json_with_all_sections() {
        let mut s = sim();
        let (_, j) = get(&mut s, "/api/state");
        for k in [
            "time",
            "telemetry",
            "metrics",
            "ble",
            "trainer",
            "settings",
            "system",
        ] {
            assert!(j.get(k).is_some(), "missing {k}");
        }
        assert_eq!(j["metrics"]["power"]["generator"]["mode"], "manual");
        assert_eq!(j["metrics"]["speed"]["generator"]["mode"], "manual");
        assert_eq!(j["settings"]["deviceName"], "DebugTrainer");
    }
}
