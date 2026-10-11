//! Build the callback planner fixture wasm for `include_bytes!` in tests.
//!
//! Isolated target dir under OUT_DIR avoids deadlocking the parent cargo flock.
//! Set `GENTS_SKIP_CALLBACK_WASM_BUILD=1` to emit a stub module (check-only /
//! no wasm32 target). Tests that need a real planner skip when the stub is used.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_PACKAGE: &str = "gents-callback-fixture-create-workspace";
const FIXTURE_ARTIFACT: &str = "gents-callback-fixture-create-workspace.wasm";
const FIXTURE_ENV: &str = "GENTS_CALLBACK_FIXTURE_CREATE_WORKSPACE_WASM_PATH";

fn main() {
    let workspace_root = workspace_root();
    let fixture_dir = workspace_root
        .join("crates")
        .join("gents-callbacks")
        .join("fixture_create_workspace");
    println!(
        "cargo:rerun-if-changed={}",
        fixture_dir.join("src").join("lib.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        fixture_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        fixture_dir.join("src").join("main.rs").display()
    );
    println!("cargo:rerun-if-env-changed=GENTS_SKIP_CALLBACK_WASM_BUILD");

    if env::var("GENTS_SKIP_CALLBACK_WASM_BUILD").is_ok() {
        emit_stub(FIXTURE_ENV, "fixture_create_workspace_stub.wasm");
        return;
    }

    build_fixture(
        &workspace_root,
        FIXTURE_PACKAGE,
        FIXTURE_ARTIFACT,
        FIXTURE_ENV,
    );
}

fn build_fixture(workspace_root: &Path, pkg: &str, artifact_name: &str, env_var: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let wasm_target_dir = out_dir.join("callback-wasm-target");

    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            pkg,
            "--bin",
            pkg,
            "--target",
            "wasm32-wasip1",
            "--config",
            "profile.dev.panic=\"abort\"",
            "--target-dir",
        ])
        .arg(&wasm_target_dir)
        .current_dir(workspace_root)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            panic!(
                "callback fixture wasm build for {pkg} failed with {s}; install \
                 wasm32-wasip1 (`rustup target add wasm32-wasip1`) \
                 or set GENTS_SKIP_CALLBACK_WASM_BUILD=1"
            );
        }
        Err(e) => panic!("failed to spawn cargo for callback fixture wasm build: {e}"),
    }

    let artifact = wasm_target_dir
        .join("wasm32-wasip1")
        .join("debug")
        .join(artifact_name);
    assert!(
        artifact.is_file(),
        "expected callback artifact at {}",
        artifact.display()
    );

    println!("cargo:rustc-env={}={}", env_var, artifact.display());
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
    let bytes: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    std::fs::write(&stub_path, bytes).expect("write stub WASM");
    println!("cargo:rustc-env={}={}", env_var, stub_path.display());
    println!(
        "cargo:warning=GENTS_SKIP_CALLBACK_WASM_BUILD set; using stub WASM (planner e2e will skip)"
    );
}
