//! Build the callback planner fixture wasm for `include_bytes!` in tests.
//!
//! Isolated target dir under OUT_DIR avoids deadlocking the parent cargo flock.
//! Set `GENTS_SKIP_CALLBACK_WASM_BUILD=1` to emit a stub module (check-only /
//! no wasm32 target). Tests that need a real planner skip when the stub is used.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE_PACKAGE: &str = "gents-callback-fixture-create-workspace";
const FIXTURE_ARTIFACT: &str = "gents_callback_fixture_create_workspace.wasm";
const FIXTURE_ENV: &str = "GENTS_CALLBACK_FIXTURE_CREATE_WORKSPACE_WASM_PATH";

fn main() {
    let workspace_root = workspace_root();
    generate_bundled_graph_packages(&workspace_root);
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

fn generate_bundled_graph_packages(workspace_root: &Path) {
    let root = workspace_root.join("packs");
    let catalog_path = root.join("catalog.json");
    println!("cargo:rerun-if-changed={}", catalog_path.display());
    let catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(catalog_path).expect("pack catalog"))
            .expect("catalog JSON");
    assert_eq!(catalog["catalog_version"], 1);
    let mut packages: Vec<_> = catalog["packs"]
        .as_array()
        .expect("registered pack names")
        .iter()
        .map(|value| value.as_str().expect("pack name").to_owned())
        .collect();
    packages.sort();
    assert!(
        packages.windows(2).all(|pair| pair[0] != pair[1]),
        "duplicate pack registration"
    );

    let mut names = Vec::new();
    let mut graph_names = Vec::new();
    let mut arms = Vec::new();
    for name in packages {
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
            "snake_case pack name"
        );
        let package = root.join(&name);
        names.push(name.clone());
        let manifest_path = package.join("manifest.json");
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&manifest_path).expect("pack manifest must exist"),
        )
        .expect("pack manifest must be JSON");
        assert_eq!(
            manifest["name"].as_str(),
            Some(name.as_str()),
            "pack directory/name mismatch"
        );
        if manifest["kind"] == "graph" {
            graph_names.push(name.clone());
        }
        let mut files = Vec::new();
        files.push(("manifest.json".to_owned(), manifest_path));
        let canonical_root = package.canonicalize().expect("canonical pack root");
        for asset in manifest["assets"].as_array().expect("declared pack assets") {
            let relative = asset.as_str().expect("asset path string");
            assert!(
                Path::new(relative)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "asset must be a relative normal path"
            );
            assert!(
                !relative
                    .split('/')
                    .any(|p| p.starts_with('.') || p == "runs"),
                "private/run assets must not be bundled"
            );
            let absolute = package
                .join(relative)
                .canonicalize()
                .expect("declared asset exists");
            assert!(
                absolute.starts_with(&canonical_root),
                "asset escapes package"
            );
            files.push((relative.to_owned(), absolute));
        }
        files.sort();
        for (relative, absolute) in files {
            println!("cargo:rerun-if-changed={}", absolute.display());
            arms.push(format!(
                "        ({name:?}, {relative:?}) => Some(include_bytes!({absolute:?})),",
                absolute = absolute.to_string_lossy(),
            ));
        }
    }

    let generated = format!(
        "pub(crate) const BUNDLED_PACK_NAMES: &[&str] = &{names:?};\n\
         pub(crate) const BUNDLED_GRAPH_PACKAGE_NAMES: &[&str] = &{graph_names:?};\n\
         pub(crate) fn bundled_graph_package_asset(package: &str, path: &str) -> Option<&'static [u8]> {{\n\
             match (package, path) {{\n{}\n\
                 _ => None,\n\
             }}\n\
         }}\n",
        arms.join("\n"),
    );
    let output =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("bundled_graph_packages.rs");
    std::fs::write(output, generated).expect("write bundled graph package inventory");
}

fn build_fixture(workspace_root: &Path, pkg: &str, artifact_name: &str, env_var: &str) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let wasm_target_dir = out_dir.join("callback-wasm-target");

    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            pkg,
            "--target",
            "wasm32-unknown-unknown",
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
                 wasm32-unknown-unknown (`rustup target add wasm32-unknown-unknown`) \
                 or set GENTS_SKIP_CALLBACK_WASM_BUILD=1"
            );
        }
        Err(e) => panic!("failed to spawn cargo for callback fixture wasm build: {e}"),
    }

    let artifact = wasm_target_dir
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join(artifact_name);
    let alt = wasm_target_dir
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join(format!("lib{artifact_name}"));
    let pkg_alt = wasm_target_dir
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join(format!("{}.wasm", pkg.replace('-', "_")));

    let path = if artifact.exists() {
        artifact
    } else if alt.exists() {
        alt
    } else if pkg_alt.exists() {
        pkg_alt
    } else {
        let dir = wasm_target_dir.join("wasm32-unknown-unknown").join("debug");
        let listing = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|_| "<unreadable>".into());
        panic!(
            "expected WASM artifact at {}, {}, or {}, directory contains: [{listing}]",
            artifact.display(),
            alt.display(),
            pkg_alt.display()
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
    let bytes: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    std::fs::write(&stub_path, bytes).expect("write stub WASM");
    println!("cargo:rustc-env={}={}", env_var, stub_path.display());
    println!(
        "cargo:warning=GENTS_SKIP_CALLBACK_WASM_BUILD set; using stub WASM (planner e2e will skip)"
    );
}
