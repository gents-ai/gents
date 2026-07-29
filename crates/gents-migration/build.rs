//! Build fixture (and future production) lens wasm artifacts for `include_bytes!`.
//!
//! Isolated target dir under OUT_DIR avoids deadlocking the parent cargo flock.
//! Set `GENTS_SKIP_LENS_BUILD=1` to emit a stub module (check-only / no wasm target).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_PACKAGE: &str = "gents-lens-fixture-add-label";
const FIXTURE_ARTIFACT: &str = "gents_lens_fixture_add_label.wasm";
const FIXTURE_ENV: &str = "GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH";

fn main() {
    let workspace_root = workspace_root();
    let lens_dir = workspace_root
        .join("crates")
        .join("gents-lenses")
        .join("fixture_add_label");
    println!(
        "cargo:rerun-if-changed={}",
        lens_dir.join("src").join("lib.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        lens_dir.join("Cargo.toml").display()
    );
    println!("cargo:rerun-if-env-changed=GENTS_SKIP_LENS_BUILD");

    if env::var("GENTS_SKIP_LENS_BUILD").is_ok() {
        emit_stub(FIXTURE_ENV, "fixture_add_label_stub.wasm");
        return;
    }

    build_lens(
        &workspace_root,
        FIXTURE_PACKAGE,
        FIXTURE_ARTIFACT,
        FIXTURE_ENV,
    );
}

fn build_lens(workspace_root: &Path, pkg: &str, artifact_name: &str, env_var: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let lens_target_dir = out_dir.join("lens-target");

    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            pkg,
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&lens_target_dir)
        .current_dir(workspace_root)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            panic!(
                "lens wasm build for {pkg} failed with {s}; install wasm32-unknown-unknown \
                 (`rustup target add wasm32-unknown-unknown`) or set GENTS_SKIP_LENS_BUILD=1"
            );
        }
        Err(e) => panic!("failed to spawn cargo for lens build: {e}"),
    }

    // cdylib artifact name uses underscores from the package name.
    let artifact = lens_target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(artifact_name);

    // cargo may emit lib{name}.wasm depending on crate name mangling
    let alt = lens_target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(format!(
            "{}.wasm",
            pkg.replace('-', "_")
        ));

    let path = if artifact.exists() {
        artifact
    } else if alt.exists() {
        alt
    } else {
        // list dir for diagnostics
        let dir = lens_target_dir
            .join("wasm32-unknown-unknown")
            .join("release");
        let listing = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "<unreadable>".into());
        panic!(
            "expected WASM artifact at {} or {}, directory contains: [{listing}]",
            artifact.display(),
            alt.display()
        );
    };

    println!("cargo:rustc-env={}={}", env_var, path.display());
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("two parents above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn emit_stub(env_var: &str, stub_name: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let stub_path = out_dir.join(stub_name);
    // Minimal valid WASM module header: magic + version.
    let bytes: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    std::fs::write(&stub_path, bytes).expect("write stub WASM");
    println!("cargo:rustc-env={}={}", env_var, stub_path.display());
    println!(
        "cargo:warning=GENTS_SKIP_LENS_BUILD set; using stub WASM (lens e2e will not transform)"
    );
}
