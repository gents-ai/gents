//! Binary-size bench: what linking regolith costs, in bytes.
//!
//! Custom harness, not criterion. Two helper crates are generated into a
//! scratch directory and built at run time on the same release profile: a
//! baseline that only argues and prints, and the same program plus a database
//! it writes to, reads from, and iterates. regolith's contribution is the
//! difference, so the std and runtime cost every Rust binary pays is
//! subtracted out rather than attributed to regolith.
//!
//! The figure covers the surface the probe touches, and no more: fat LTO plus
//! a stripped binary drops everything else, so the surface is reported next to
//! the number and widening it would raise the number.
//!
//! A target that is not installed, or a missing `wasm-opt`, is reported as an
//! absent family with the reason. No estimate is ever substituted.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const WASM_TARGET: &str = "wasm32-wasip1";
/// This toolchain emits these proposals by default, so wasm-opt has to accept
/// them or it rejects the module before it optimizes anything.
const WASM_OPT_FLAGS: [&str; 4] = [
    "-Oz",
    "--enable-bulk-memory",
    "--enable-sign-ext",
    "--enable-nontrapping-float-to-int",
];
const PROFILE: &str = "opt-level=3,lto=fat,codegen-units=1,strip=true,panic=abort";
const SURFACE: &str = "open,put,get,iter";

const CARGO_TOML: &str = "[package]\n\
                          name = \"NAME\"\n\
                          version = \"0.0.0\"\n\
                          edition = \"2021\"\n\
                          publish = false\n\
                          \n\
                          [workspace]\n\
                          \n\
                          [dependencies]\n\
                          DEPS\n\
                          \n\
                          [profile.release]\n\
                          opt-level = 3\n\
                          lto = \"fat\"\n\
                          codegen-units = 1\n\
                          strip = true\n\
                          panic = \"abort\"\n";

/// Not `fn main() {}`: a truly empty main links neither `fmt` nor stdout, and
/// the difference would then charge regolith for std machinery every real program
/// already carries. The baseline argues and prints so both sides start from a
/// program that has done some work.
const BASELINE_MAIN: &str = "fn main() { println!(\"{}\", std::env::args().count()); }\n";

/// Held to the surface a typical embedded dependent uses. Fat LTO plus strip
/// means only what this touches survives the link, so widening it raises the
/// number: it is the cost of this surface, not of every API regolith exposes.
const LINKED_MAIN: &str = r#"use regolith::{Db, Options};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "regolith-size-probe".into());
    if let Ok(db) = Db::open(&path, Options::default()) {
        let _ = db.put(b"k", b"v");
        let _ = db.get(b"k");
        let mut it = db.iter();
        it.seek_to_first();
        while it.valid() {
            it.next();
        }
    }
}
"#;

struct Cfg {
    dry_run: bool,
    native_only: bool,
    keep: bool,
    target_dir: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: size [--dry-run|--test] [--native-only] [--keep] [--target-dir <path>]\n\
     \x20--dry-run generates the helper crates and probes availability without building,\n\
     \x20and writes no metric family because nothing was measured."
}

impl Cfg {
    fn parse(args: Vec<String>) -> Cfg {
        let mut c = Cfg {
            dry_run: false,
            native_only: false,
            keep: false,
            target_dir: None,
        };
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--bench" => {}
                "--dry-run" | "--test" => c.dry_run = true,
                "--native-only" => c.native_only = true,
                "--keep" => c.keep = true,
                "--target-dir" => {
                    let v = it
                        .next()
                        .unwrap_or_else(|| panic!("--target-dir needs a path\n{}", usage()));
                    c.target_dir = Some(PathBuf::from(v));
                }
                "-h" | "--help" => {
                    println!("{}", usage());
                    std::process::exit(0);
                }
                o => {
                    eprintln!("size: unknown argument {o:?}\n{}", usage());
                    std::process::exit(2);
                }
            }
        }
        c
    }
}

/// The printed summary and the JSON record are built from one field list, so a
/// human reading the log and a tool reading the file cannot be told different
/// numbers. Returns the JSON object for the row it just printed.
fn emit(section: &str, kv: &[(&str, String)]) -> String {
    let line: Vec<String> = kv.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("size.{section} {}", line.join(" "));
    let obj: Vec<String> = kv
        .iter()
        .map(|(k, v)| format!("\"{k}\":{}", json_value(v)))
        .collect();
    format!("{{{}}}", obj.join(","))
}

/// Numbers, booleans and null go through bare; everything else becomes a JSON
/// string. The digit filter keeps `inf` and `NaN` out, which JSON cannot spell.
fn json_value(v: &str) -> String {
    if v == "true" || v == "false" || v == "null" {
        return v.to_string();
    }
    let numeric = !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
        && v.parse::<f64>().is_ok();
    if numeric {
        v.to_string()
    } else {
        format!("\"{v}\"")
    }
}

/// Values live on a whitespace-separated `k=v` grid and inside JSON strings, so
/// a free-form reason loses whatever would split either one.
fn flatten(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .map(|c| if c == '"' || c == '\\' { '_' } else { c })
        .take(200)
        .collect()
}

fn write_crate(root: &Path, name: &str, deps: &str, main: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("src")).unwrap_or_else(|e| panic!("mkdir {}: {e}", dir.display()));
    let toml = CARGO_TOML.replace("NAME", name).replace("DEPS", deps);
    fs::write(dir.join("Cargo.toml"), toml).unwrap_or_else(|e| panic!("write Cargo.toml: {e}"));
    fs::write(dir.join("src/main.rs"), main).unwrap_or_else(|e| panic!("write main.rs: {e}"));
    dir.join("Cargo.toml")
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn rustc_bin() -> String {
    std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string())
}

fn host_triple() -> String {
    match Command::new(rustc_bin()).arg("-vV").output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("host: ").map(|h| h.trim().to_string()))
            .unwrap_or_else(|| "unknown".to_string()),
        Err(_) => "unknown".to_string(),
    }
}

fn probe_target(triple: &str) -> Result<(), String> {
    let out = Command::new(rustc_bin())
        .args(["--print", "target-libdir", "--target", triple])
        .output()
        .map_err(|e| format!("running rustc: {e}"))?;
    if !out.status.success() {
        return Err(format!("rustc rejects target {triple}"));
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if Path::new(&dir).is_dir() {
        Ok(())
    } else {
        Err(format!(
            "std for {triple} is not installed (no {dir}); rustup target add {triple}"
        ))
    }
}

fn probe_wasm_opt() -> Result<String, String> {
    let out = Command::new("wasm-opt")
        .arg("--version")
        .output()
        .map_err(|e| format!("wasm-opt not runnable: {e}"))?;
    if !out.status.success() {
        return Err("wasm-opt --version failed".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn build(manifest: &Path, target_dir: &Path, triple: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new(cargo_bin());
    cmd.arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir);
    if let Some(t) = triple {
        cmd.args(["--target", t]);
    }
    // The outer `cargo bench` leaves its own build configuration in this
    // process; inheriting it would silently change the profile being measured.
    for var in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_MANIFEST_DIR",
        "CARGO_PRIMARY_PACKAGE",
        "CARGO_MAKEFLAGS",
        "MAKEFLAGS",
    ] {
        cmd.env_remove(var);
    }
    let out = cmd.output().map_err(|e| format!("running cargo: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let reason = stderr
        .lines()
        .find(|l| l.starts_with("error"))
        .unwrap_or_else(|| stderr.lines().last().unwrap_or("cargo build failed"));
    Err(reason.to_string())
}

fn artifact(target_dir: &Path, triple: Option<&str>, name: &str, ext: &str) -> PathBuf {
    let mut p = target_dir.to_path_buf();
    if let Some(t) = triple {
        p.push(t);
    }
    p.push("release");
    p.push(format!("{name}{ext}"));
    p
}

fn file_bytes(p: &Path) -> Result<u64, String> {
    fs::metadata(p)
        .map(|m| m.len())
        .map_err(|e| format!("stat {}: {e}", p.display()))
}

fn run_wasm_opt(input: &Path, output: &Path) -> Result<u64, String> {
    let out = Command::new("wasm-opt")
        .args(WASM_OPT_FLAGS)
        .arg(input)
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|e| format!("running wasm-opt: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "wasm-opt failed: {}",
            stderr.lines().last().unwrap_or("no output")
        ));
    }
    file_bytes(output)
}

fn build_pair(
    baseline_manifest: &Path,
    linked_manifest: &Path,
    target_dir: &Path,
    triple: Option<&str>,
    ext: &str,
) -> Result<(u64, u64), String> {
    build(baseline_manifest, target_dir, triple)?;
    build(linked_manifest, target_dir, triple)?;
    let baseline = file_bytes(&artifact(target_dir, triple, "baseline", ext))?;
    let linked = file_bytes(&artifact(target_dir, triple, "linked", ext))?;
    Ok((baseline, linked))
}

fn optimize_pair(target_dir: &Path, out_dir: &Path) -> Result<(u64, u64), String> {
    let baseline_in = artifact(target_dir, Some(WASM_TARGET), "baseline", ".wasm");
    let linked_in = artifact(target_dir, Some(WASM_TARGET), "linked", ".wasm");
    let baseline = run_wasm_opt(&baseline_in, &out_dir.join("baseline.opt.wasm"))?;
    let linked = run_wasm_opt(&linked_in, &out_dir.join("linked.opt.wasm"))?;
    Ok((baseline, linked))
}

fn emit_family(name: &str, result: &Result<(u64, u64), String>) -> String {
    let kv = match result {
        Ok((baseline, linked)) => {
            let regolith = linked.saturating_sub(*baseline);
            vec![
                ("name", name.to_string()),
                ("available", "true".to_string()),
                ("baseline_bytes", baseline.to_string()),
                ("linked_bytes", linked.to_string()),
                ("regolith_bytes", regolith.to_string()),
                ("regolith_kib", format!("{:.1}", regolith as f64 / 1024.0)),
                ("reason", "null".to_string()),
            ]
        }
        Err(reason) => vec![
            ("name", name.to_string()),
            ("available", "false".to_string()),
            ("baseline_bytes", "null".to_string()),
            ("linked_bytes", "null".to_string()),
            ("regolith_bytes", "null".to_string()),
            ("regolith_kib", "null".to_string()),
            ("reason", flatten(reason)),
        ],
    };
    emit("family", &kv)
}

fn main() {
    let cfg = Cfg::parse(std::env::args().skip(1).collect());
    let regolith_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scratch = common::TempDb::new("size");
    let root = scratch.path().to_path_buf();

    let dep = format!(
        "regolith = {{ path = \"{}\" }}",
        regolith_dir.display().to_string().replace('\\', "\\\\")
    );
    let baseline_manifest = write_crate(&root, "baseline", "", BASELINE_MAIN);
    let linked_manifest = write_crate(&root, "linked", &dep, LINKED_MAIN);
    let target_dir = cfg
        .target_dir
        .clone()
        .unwrap_or_else(|| root.join("target"));

    let host = host_triple();
    println!("regolith binary-size bench (bytes, not time)");
    println!("size.meta host={host} profile={PROFILE} surface={SURFACE}");
    println!(
        "size.meta scratch={} target_dir={}",
        root.display(),
        target_dir.display()
    );

    let wasm_target = probe_target(WASM_TARGET);
    let wasm_opt = probe_wasm_opt();
    println!(
        "size.probe target={WASM_TARGET} installed={} wasm_opt={}",
        wasm_target.is_ok(),
        match &wasm_opt {
            Ok(v) => flatten(v),
            Err(e) => flatten(e),
        }
    );

    if cfg.dry_run {
        println!("size.dry_run helper crates generated, nothing built, no family written");
        if cfg.keep {
            println!("size.kept scratch={}", root.display());
            std::mem::forget(scratch);
        }
        return;
    }

    let native = build_pair(&baseline_manifest, &linked_manifest, &target_dir, None, "");

    let (wasm, wasm_opted) = if cfg.native_only {
        let skipped: Result<(u64, u64), String> = Err("skipped by --native-only".to_string());
        (skipped.clone(), skipped)
    } else {
        match &wasm_target {
            Err(e) => (Err(e.clone()), Err(e.clone())),
            Ok(()) => {
                let built = build_pair(
                    &baseline_manifest,
                    &linked_manifest,
                    &target_dir,
                    Some(WASM_TARGET),
                    ".wasm",
                );
                let opted = match (&built, &wasm_opt) {
                    (Ok(_), Ok(_)) => optimize_pair(&target_dir, &root),
                    (Ok(_), Err(e)) => Err(e.clone()),
                    (Err(e), _) => Err(e.clone()),
                };
                (built, opted)
            }
        }
    };

    let rows = [
        emit_family("native", &native),
        emit_family(WASM_TARGET, &wasm),
        emit_family("wasm32-wasip1+wasm-opt-Oz", &wasm_opted),
    ];

    let data = format!(
        "{{\"host\":\"{host}\",\"profile\":\"{PROFILE}\",\"surface\":\"{SURFACE}\",\
         \"wasm_opt\":{},\"families\":[{}]}}",
        match &wasm_opt {
            Ok(v) => format!("\"{}\"", flatten(v)),
            Err(_) => "null".to_string(),
        },
        rows.join(",")
    );
    common::write_family("size", &data);

    if cfg.keep {
        println!("size.kept scratch={}", root.display());
        std::mem::forget(scratch);
    }
}
