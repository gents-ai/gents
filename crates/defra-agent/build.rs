//! Build the WASM lens artifact before defra-agent compiles, so
//! migration.rs can `include_bytes!` it.
//!
//! The WASM target requires the wasm32-unknown-unknown rust target. If it's
//! not installed, this script prints a helpful error and fails.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const LENS_PACKAGE: &str = "agent-tool-call-lifecycle-v1-to-v2-lens";

fn main() {
    // Re-run this build script if the lens crate's source changes.
    let workspace_root = workspace_root();
    let lens_dir = workspace_root
        .join("crates")
        .join("defra-agent-lenses")
        .join("agent_tool_call_lifecycle_v1_to_v2");
    println!(
        "cargo:rerun-if-changed={}",
        lens_dir.join("src").join("lib.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        lens_dir.join("Cargo.toml").display()
    );

    // Skip the WASM build when running rustdoc or in environments without the
    // wasm32 target. The result is a build that compiles defra-agent for
    // syntax/type checking but produces a defra-agent that will panic at
    // startup if it actually tries to register the lens. Reasonable trade-off
    // for `cargo doc` and similar local-dev paths; production builds always
    // have the WASM target.
    if env::var("DEFRA_AGENT_SKIP_LENS_BUILD").is_ok() {
        // Emit a stub so include_bytes! has something to find.
        emit_stub_artifact(&workspace_root);
        return;
    }

    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            LENS_PACKAGE,
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .current_dir(&workspace_root)
        .status();

    let status = match status {
        Ok(s) => s,
        Err(err) => {
            panic!(
                "failed to invoke cargo to build {LENS_PACKAGE}: {err}.\n\
                 If the wasm32-unknown-unknown target is not installed, run:\n\
                 \trustup target add wasm32-unknown-unknown\n\
                 To skip the lens build (e.g. for `cargo doc`), set \
                 DEFRA_AGENT_SKIP_LENS_BUILD=1."
            );
        }
    };

    if !status.success() {
        panic!(
            "cargo build for {LENS_PACKAGE} failed (status {status}).\n\
             If the wasm32-unknown-unknown target is missing, run:\n\
             \trustup target add wasm32-unknown-unknown"
        );
    }

    let artifact = workspace_root
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("agent_tool_call_lifecycle_v1_to_v2_lens.wasm");

    if !artifact.exists() {
        panic!(
            "expected WASM artifact at {} but it was not produced by the build",
            artifact.display()
        );
    }

    // Emit the artifact path for migration.rs to include_bytes! against.
    println!(
        "cargo:rustc-env=AGENT_TOOL_CALL_LIFECYCLE_V1_TO_V2_LENS_WASM_PATH={}",
        artifact.display()
    );
}

fn workspace_root() -> PathBuf {
    // Walk up from CARGO_MANIFEST_DIR (crates/defra-agent) two levels.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("two parents above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn emit_stub_artifact(workspace_root: &PathBuf) {
    // Write a minimal valid WASM module to a build-out path so include_bytes!
    // succeeds; the runtime will refuse to use it.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let stub_path = out_dir.join("lens_stub.wasm");
    // Minimal valid WASM module header: magic + version.
    let bytes: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    std::fs::write(&stub_path, bytes).expect("write stub WASM");
    println!(
        "cargo:rustc-env=AGENT_TOOL_CALL_LIFECYCLE_V1_TO_V2_LENS_WASM_PATH={}",
        stub_path.display()
    );
    println!("cargo:warning=DEFRA_AGENT_SKIP_LENS_BUILD set; using stub WASM (lens will not function at runtime).");

    let _ = workspace_root; // suppress unused warning
}
