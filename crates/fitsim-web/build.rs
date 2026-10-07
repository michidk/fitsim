//! Gzips everything under `web/` and generates a static asset table.
//!
//! The frontend is plain HTML/CSS/ES-modules (no bundler, no node step) so the firmware build
//! only needs Rust. Assets are compressed once at build time and served with
//! `Content-Encoding: gzip`, which every browser supports.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).expect("read web dir").flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, root, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((format!("/{rel}"), path));
        }
    }
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" | "webmanifest" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let web = manifest.join("../../web");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed={}", web.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files = Vec::new();
    collect(&web, &web, &mut files);
    assert!(
        files.iter().any(|(p, _)| p == "/index.html"),
        "web/index.html is missing"
    );

    let mut table = String::from("&[\n");
    let (mut raw_total, mut gz_total) = (0usize, 0usize);
    for (i, (url, path)) in files.iter().enumerate() {
        println!("cargo:rerun-if-changed={}", path.display());
        let data = fs::read(path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&data).unwrap();
        let gz = enc.finish().unwrap();
        raw_total += data.len();
        gz_total += gz.len();
        let gz_path = out_dir.join(format!("asset{i}.gz"));
        fs::write(&gz_path, &gz).unwrap();
        writeln!(
            table,
            "    Asset {{ path: {url:?}, mime: {:?}, gz: include_bytes!({:?}) }},",
            mime(url),
            gz_path.to_string_lossy()
        )
        .unwrap();
    }
    table.push_str("]\n");
    fs::write(out_dir.join("assets.rs"), table).unwrap();
    println!(
        "cargo:warning=web assets: {} files, {} B raw -> {} B gzip",
        files.len(),
        raw_total,
        gz_total
    );
}
