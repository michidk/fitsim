//! Desktop build of the fitness simulator.
//!
//! Runs the same `Simulator`, REST router and WebSocket stream as the firmware, minus the radio.
//! Use it to develop the web UI and to script against the API without hardware:
//!
//! ```text
//! cargo run -p fitsim-host -- --port 8080
//! ```
//!
//! Extra endpoints only available here (they stand in for a BLE client):
//!
//! * `POST /api/debug/ble`  `{"event":"connect"|"disconnect"|"subscribe"|"unsubscribe","client":64,"char":"indoorBikeData"}`
//! * `POST /api/debug/ftms-write`  `{"client":64,"hex":"0500c800"}` -> `{"response":"800501", ...}`

use std::net::TcpListener;
use std::path::PathBuf;

use fitsim_host::{Options, serve};

fn parse_args() -> Options {
    let mut o = Options {
        port: 8080,
        data_dir: PathBuf::from(".fitsim-data"),
        provisioning: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" | "-p" => {
                o.port = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--port needs a number")
            }
            "--data-dir" => {
                o.data_dir = args
                    .next()
                    .map(PathBuf::from)
                    .expect("--data-dir needs a path")
            }
            "--provisioning" => o.provisioning = true,
            "--help" | "-h" => {
                println!("fitsim-host [--port 8080] [--data-dir .fitsim-data] [--provisioning]");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    o
}

fn main() {
    let opts = parse_args();
    let listener = TcpListener::bind(("0.0.0.0", opts.port))
        .unwrap_or_else(|e| panic!("cannot bind port {}: {e}", opts.port));
    println!(
        "fitness simulator (host) listening on http://localhost:{}",
        opts.port
    );
    serve(listener, opts);
}
