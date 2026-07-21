//! Build the WASM lens artifacts before gents compiles, so
//! migration.rs can `include_bytes!` them.
//!
//! The WASM target requires the wasm32-unknown-unknown rust target. If it's
//! not installed, this script prints a helpful error and fails.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const LENS_TOOL_CALL_PACKAGE: &str = "agent-tool-call-lifecycle-v1-to-v2-lens";
const LENS_SUBAGENT_PACKAGE: &str = "agent-subagent-v2-to-v3-lens";

fn main() {
    let workspace_root = workspace_root();

    // Re-run this build script if either lens crate's source changes.
    for (subdir, _pkg) in [
        ("agent_tool_call_lifecycle_v1_to_v2", LENS_TOOL_CALL_PACKAGE),
        ("agent_subagent_v2_to_v3", LENS_SUBAGENT_PACKAGE),
    ] {
        let lens_dir = workspace_root
            .join("crates")
            .join("gents-lenses")
            .join(subdir);
        println!(
            "cargo:rerun-if-changed={}",
            lens_dir.join("src").join("lib.rs").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            lens_dir.join("Cargo.toml").display()
        );
    }

    // Skip the WASM build when running rustdoc or in environments without the
    // wasm32 target. The result is a build that compiles gents for
    // syntax/type checking but produces a gents that will panic at
    // startup if it actually tries to register the lens. Reasonable trade-off
    // for `cargo doc` and similar local-dev paths; production builds always
    // have the WASM target.
    if env::var("GENTS_SKIP_LENS_BUILD").is_ok() {
        emit_stub_artifact(
            &workspace_root,
            "AGENT_TOOL_CALL_LIFECYCLE_V1_TO_V2_LENS_WASM_PATH",
            "tool_call_lens_stub.wasm",
        );
        emit_stub_artifact(
            &workspace_root,
            "AGENT_SUBAGENT_V2_TO_V3_LENS_WASM_PATH",
            "subagent_lens_stub.wasm",
        );
        return;
    }

    build_lens(
        &workspace_root,
        LENS_TOOL_CALL_PACKAGE,
        "agent_tool_call_lifecycle_v1_to_v2_lens.wasm",
        "AGENT_TOOL_CALL_LIFECYCLE_V1_TO_V2_LENS_WASM_PATH",
    );
    build_lens(
        &workspace_root,
        LENS_SUBAGENT_PACKAGE,
        "agent_subagent_v2_to_v3_lens.wasm",
        "AGENT_SUBAGENT_V2_TO_V3_LENS_WASM_PATH",
    );
}

fn build_lens(workspace_root: &Path, pkg: &str, artifact_name: &str, env_var: &str) {
    // Build the lens with an isolated target directory. This nested `cargo`
    // invocation must not share the parent build's target dir: the outer
    // release `cargo` already holds an exclusive flock on
    // `target/release/.cargo-build-lock`, and a nested `cargo` pointed at the
    // same target dir blocks forever in flock() waiting for that lock — a
    // hard deadlock (e.g. release CI hanging indefinitely at 0% CPU). Giving
    // the inner build its own target dir under OUT_DIR removes the contention.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let lens_target_dir = out_dir.join("lens-target");

    let status = Command::new("cargo")
        .args([
            "build",
            "--locked",
            "-p",
            pkg,
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .env("CARGO_TARGET_DIR", &lens_target_dir)
        .current_dir(workspace_root)
        .status();

    let status = match status {
        Ok(s) => s,
        Err(err) => {
            panic!(
                "failed to invoke cargo to build {pkg}: {err}.\n\
                 If the wasm32-unknown-unknown target is not installed, run:\n\
                 \trustup target add wasm32-unknown-unknown\n\
                 To skip the lens build (e.g. for `cargo doc`), set \
                 GENTS_SKIP_LENS_BUILD=1."
            );
        }
    };

    if !status.success() {
        panic!(
            "cargo build for {pkg} failed (status {status}).\n\
             If the wasm32-unknown-unknown target is missing, run:\n\
             \trustup target add wasm32-unknown-unknown"
        );
    }

    let artifact = lens_target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join(artifact_name);

    if !artifact.exists() {
        panic!(
            "expected WASM artifact at {} but it was not produced by the build",
            artifact.display()
        );
    }

    println!("cargo:rustc-env={}={}", env_var, artifact.display());
}

fn workspace_root() -> PathBuf {
    // Walk up from CARGO_MANIFEST_DIR (crates/gents) two levels.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("two parents above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn emit_stub_artifact(workspace_root: &Path, env_var: &str, stub_name: &str) {
    // Write a minimal valid WASM module to a build-out path so include_bytes!
    // succeeds; the runtime will refuse to use it.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let stub_path = out_dir.join(stub_name);
    // Minimal valid WASM module header: magic + version.
    let bytes: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    std::fs::write(&stub_path, bytes).expect("write stub WASM");
    println!("cargo:rustc-env={}={}", env_var, stub_path.display());
    println!("cargo:warning=GENTS_SKIP_LENS_BUILD set; using stub WASM (lens will not function at runtime).");

    let _ = workspace_root; // suppress unused warning
}
