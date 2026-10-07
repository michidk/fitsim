//! Embedded web frontend plus the top-level HTTP router.
//!
//! [`route`] is shared by the firmware and the host dev server: it serves the gzip-compressed
//! single-page app, forwards `/api/*` to [`fitsim_core::api`], and implements the captive-portal
//! redirects used while the device is in Wi-Fi provisioning mode.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use fitsim_core::api::{self, Output, Request};
use fitsim_core::http::{Method, Response};
use fitsim_core::net::AP_IP;
use fitsim_core::sim::Simulator;

pub struct Asset {
    pub path: &'static str,
    pub mime: &'static str,
    pub gz: &'static [u8],
}

/// All files from `web/`, gzip-compressed at build time.
pub static ASSETS: &[Asset] = include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Looks up an asset. `/` maps to `index.html`; extension-less paths fall back to it too so the
/// single-page app can use clean URLs.
pub fn find(path: &str) -> Option<&'static Asset> {
    let path = if path == "/" || path.is_empty() {
        "/index.html"
    } else {
        path
    };
    if let Some(a) = ASSETS.iter().find(|a| a.path == path) {
        return Some(a);
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if !last.contains('.') {
        ASSETS.iter().find(|a| a.path == "/index.html")
    } else {
        None
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct RouteContext {
    /// True while the setup access point is active: unknown URLs are redirected to the portal.
    pub captive_portal: bool,
}

fn redirect_to_portal() -> Response {
    let loc = alloc::format!(
        "http://{}.{}.{}.{}/",
        AP_IP[0],
        AP_IP[1],
        AP_IP[2],
        AP_IP[3]
    );
    let mut r = Response::new(302, "text/plain", alloc::vec::Vec::new());
    r.redirect = Some(loc);
    r
}

/// Routes one request. Platform-specific endpoints (Wi-Fi scan, host debug hooks) must be handled
/// by the caller *before* this.
pub fn route(sim: &mut Simulator, req: &Request<'_>, ctx: RouteContext) -> Output {
    let (path, method) = (req.path, req.method);
    if path == "/api" || path.starts_with("/api/") {
        return api::handle(sim, req);
    }
    if method == Method::Options {
        return Output {
            response: fitsim_core::http::preflight(),
            effects: alloc::vec::Vec::new(),
        };
    }
    if !matches!(method, Method::Get | Method::Head) {
        return Output {
            response: Response::error(405, "method not allowed"),
            effects: alloc::vec::Vec::new(),
        };
    }
    // Captive-portal probes (Android, iOS, Windows, Firefox) all hit well-known paths.
    let probe = matches!(
        path,
        "/generate_204"
            | "/gen_204"
            | "/hotspot-detect.html"
            | "/library/test/success.html"
            | "/connecttest.txt"
            | "/ncsi.txt"
            | "/canonical.html"
            | "/success.txt"
    );
    if ctx.captive_portal && probe {
        return Output {
            response: redirect_to_portal(),
            effects: alloc::vec::Vec::new(),
        };
    }
    let response = match find(path) {
        Some(a) => Response::asset(a.mime, a.gz),
        None if ctx.captive_portal => redirect_to_portal(),
        None => Response::error(404, "not found"),
    };
    Output {
        response,
        effects: alloc::vec::Vec::new(),
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::*;
    use fitsim_core::settings::Settings;

    fn get(sim: &mut Simulator, path: &str, ctx: RouteContext) -> Response {
        route(
            sim,
            &Request {
                method: Method::Get,
                path,
                query: "",
                body: b"",
            },
            ctx,
        )
        .response
    }

    #[test]
    fn serves_the_app_and_falls_back_for_clean_urls() {
        let mut sim = Simulator::new(Settings::default(), 1);
        let index = get(&mut sim, "/", RouteContext::default());
        assert_eq!((index.status, index.gzip), (200, true));
        assert!(index.content_type.starts_with("text/html"));
        assert_eq!(
            get(&mut sim, "/trainer", RouteContext::default()).status,
            200,
            "SPA route"
        );
        assert_eq!(
            get(&mut sim, "/missing.png", RouteContext::default()).status,
            404
        );
        // Assets really are gzip streams.
        assert_eq!(&index.body.as_slice()[..2], [0x1f, 0x8b]);
    }

    #[test]
    fn api_is_dispatched_to_core() {
        let mut sim = Simulator::new(Settings::default(), 1);
        assert_eq!(
            get(&mut sim, "/api/state", RouteContext::default()).status,
            200
        );
        assert_eq!(
            get(&mut sim, "/api/nope", RouteContext::default()).status,
            404
        );
    }

    #[test]
    fn captive_portal_redirects_probes_and_unknown_urls() {
        let mut sim = Simulator::new(Settings::default(), 1);
        let ctx = RouteContext {
            captive_portal: true,
        };
        for p in [
            "/generate_204",
            "/hotspot-detect.html",
            "/connecttest.txt",
            "/whatever.png",
        ] {
            let r = get(&mut sim, p, ctx);
            assert_eq!(r.status, 302, "{p}");
            assert_eq!(r.redirect.as_deref(), Some("http://192.168.4.1/"));
        }
        assert_eq!(get(&mut sim, "/", ctx).status, 200);
        assert_eq!(get(&mut sim, "/api/state", ctx).status, 200);
        // Without the portal active nothing is redirected: extension-less paths serve the SPA,
        // unknown files 404.
        assert_eq!(
            get(&mut sim, "/generate_204", RouteContext::default()).status,
            200
        );
        assert_eq!(
            get(&mut sim, "/whatever.png", RouteContext::default()).status,
            404
        );
    }

    #[test]
    fn non_get_static_requests_are_rejected() {
        let mut sim = Simulator::new(Settings::default(), 1);
        let r = route(
            &mut sim,
            &Request {
                method: Method::Post,
                path: "/",
                query: "",
                body: b"",
            },
            RouteContext::default(),
        );
        assert_eq!(r.response.status, 405);
    }
}
