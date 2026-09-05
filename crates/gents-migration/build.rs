//! Build fixture and production lens WASM artifacts for `include_bytes!`.
//!
//! Isolated target dir under OUT_DIR avoids deadlocking the parent cargo flock.
//! GENTS_SKIP_LENS_BUILD affects only the fixture; production lenses always build.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_PACKAGE: &str = "gents-lens-fixture-add-label";
const FIXTURE_ARTIFACT: &str = "gents_lens_fixture_add_label.wasm";
const FIXTURE_ENV: &str = "GENTS_LENS_FIXTURE_ADD_LABEL_WASM_PATH";

fn main() {
    let workspace_root = workspace_root();
    for input in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root.join(input).display()
        );
    }
    println!("cargo:rerun-if-env-changed=CARGO_HOME");
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
    println!("cargo:rerun-if-changed=../gents-lenses/source_version_field.rs");
    for (directory, package, artifact, variable) in [
        (
            "workspace_capability",
            "gents-lens-workspace-capability",
            "gents_lens_workspace_capability.wasm",
            "GENTS_LENS_WORKSPACE_CAPABILITY_WASM_PATH",
        ),
        (
            "workspace_receipt_capability",
            "gents-lens-workspace-receipt-capability",
            "gents_lens_workspace_receipt_capability.wasm",
            "GENTS_LENS_WORKSPACE_RECEIPT_CAPABILITY_WASM_PATH",
        ),
    ] {
        let production = workspace_root.join("crates/gents-lenses").join(directory);
        println!(
            "cargo:rerun-if-changed={}",
            production.join("src/lib.rs").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            production.join("Cargo.toml").display()
        );
        build_lens(&workspace_root, package, artifact, variable);
    }

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

    // Match DefraDB's integration harness: lens_sdk 0.8 transport buffers are
    // corrupted by optimized WASM builds, producing null or invalid type IDs.
    // Tracked upstream: https://github.com/sourcenetwork/lens/issues/166
    let mut command = Command::new("cargo");
    command
        .args([
            "build",
            "-p",
            pkg,
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&lens_target_dir)
        .current_dir(workspace_root);
    if pkg != FIXTURE_PACKAGE {
        for (name, _) in env::vars_os() {
            if name.to_string_lossy().starts_with("CARGO_PROFILE_") {
                command.env_remove(name);
            }
        }
        command.env_remove("CARGO_INCREMENTAL");
        command.arg("--locked");
        // Transform IDs hash the full WASM bytes. Host/check-out-specific debug
        // paths and inherited outer build flags must not change production IDs.
        // Keep the existing dev optimization profile: optimized lens SDK WASM
        // has a known transport bug (see above).
        let cargo_home = env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
            .expect("Cargo home must be available for reproducible production lenses");
        let canonical_cargo_home = cargo_home
            .canonicalize()
            .unwrap_or_else(|_| cargo_home.clone());
        let compiler = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let sysroot = Command::new(compiler)
            .args(["--print", "sysroot"])
            .output()
            .expect("query compiler sysroot for production lenses");
        assert!(sysroot.status.success(), "compiler sysroot query failed");
        let sysroot = PathBuf::from(
            String::from_utf8(sysroot.stdout)
                .expect("compiler sysroot must be UTF-8")
                .trim(),
        );
        let canonical_sysroot = sysroot.canonicalize().unwrap_or_else(|_| sysroot.clone());
        let mut flags = vec![
            "-Cdebuginfo=0".to_owned(),
            "-Cstrip=symbols".to_owned(),
            format!("--remap-path-prefix={}=/gents", workspace_root.display()),
            format!("--remap-path-prefix={}=/cargo", cargo_home.display()),
            format!(
                "--remap-path-prefix={}=/cargo",
                canonical_cargo_home.display()
            ),
            format!("--remap-path-prefix={}=/rust", sysroot.display()),
            format!("--remap-path-prefix={}=/rust", canonical_sysroot.display()),
        ];
        // Runner-local Cargo homes can symlink caches to a shared home.
        // rustc embeds the resolved source paths, which lie outside that overlay.
        // Map each actual source cache to the same logical location as a normal
        // Cargo home. Keep the most specific mappings last.
        for cache in ["registry", "git"] {
            if let Ok(target) = std::fs::read_link(cargo_home.join(cache)) {
                let source = if target.is_absolute() {
                    target
                } else {
                    cargo_home.join(target)
                };
                flags.push(format!(
                    "--remap-path-prefix={}=/cargo/{cache}",
                    source.display()
                ));
            }
            if let Ok(source) = cargo_home.join(cache).canonicalize() {
                flags.push(format!(
                    "--remap-path-prefix={}=/cargo/{cache}",
                    source.display()
                ));
            }
        }
        command.env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"));
    }
    let status = command.status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            panic!(
                "lens wasm build for {pkg} failed with {s}; install wasm32-unknown-unknown \
                 (`rustup target add wasm32-unknown-unknown`) (GENTS_SKIP_LENS_BUILD skips only the test fixture)"
            );
        }
        Err(e) => panic!("failed to spawn cargo for lens build: {e}"),
    }

    // cdylib artifact name uses underscores from the package name.
    let artifact = lens_target_dir
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join(artifact_name);

    // cargo may emit lib{name}.wasm depending on crate name mangling
    let alt = lens_target_dir
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join(format!("{}.wasm", pkg.replace('-', "_")));

    let path = if artifact.exists() {
        artifact
    } else if alt.exists() {
        alt
    } else {
        // list dir for diagnostics
        let dir = lens_target_dir.join("wasm32-unknown-unknown").join("debug");
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
