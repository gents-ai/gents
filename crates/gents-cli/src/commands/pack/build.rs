//! `gents pack build`: compile a pack's tools and pack the result into one
//! `.afb`, so first-party and third-party packs alike become the artifact
//! the registry serves.
//!
//! Compiling is this module's job; packing is [`gents::pack_archive::pack_dir`]'s.
//! A tool with no `source` must already carry its compiled module; a tool
//! with a Rust crate `source` is built with `cargo` and the produced
//! `.wasm` is copied to the module path the manifest declares. Everything
//! else in the manifest travels unchanged.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::pack::{PackManifest, PackTool};
use gents::pack_archive::{pack_dir, PackAfb, PublishAs};
use serde::Serialize;
use serde_json::json;

use crate::cli::args::PackBuildArgs;

#[derive(Debug, Serialize)]
pub(crate) struct BuildReport {
    pub(crate) pack: String,
    pub(crate) version: String,
    pub(crate) artifact_digest: String,
    pub(crate) pack_digest: String,
    pub(crate) size_bytes: usize,
    pub(crate) out: PathBuf,
    pub(crate) tools: Vec<ToolReport>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ToolReport {
    pub(crate) name: String,
    pub(crate) module: String,
}

pub(crate) fn dispatch(args: PackBuildArgs) -> Result<()> {
    if args.all {
        let reports = build_all(Path::new("packs"), &args.namespace)?;
        return crate::print_json(&json!({ "packs": reports }));
    }
    let dir = args.dir.as_deref().context(
        "PACK_DIR is required; pass a directory or --all to build every pack under packs/",
    )?;
    let report = build_pack(dir, args.out.as_deref(), &args.namespace)?;
    crate::print_json(&serde_json::to_value(report)?)
}

/// Builds every pack directory under `root` (any directory that carries a
/// `manifest.json`) into a `.afb` beside it, so this repository's own packs
/// can be published to the registry in one step.
pub(crate) fn build_all(root: &Path, namespace: &str) -> Result<Vec<BuildReport>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("manifest.json").is_file())
        .collect();
    dirs.sort();
    anyhow::ensure!(
        !dirs.is_empty(),
        "no pack directories with a manifest.json were found under {}",
        root.display()
    );
    dirs.iter()
        .map(|dir| {
            build_pack(dir, None, namespace).with_context(|| format!("building {}", dir.display()))
        })
        .collect()
}

/// Compiles `dir`'s declared tools onto disk, then packs the whole
/// directory into a `.afb` at `out` (or the default sibling path).
pub(crate) fn build_pack(dir: &Path, out: Option<&Path>, namespace: &str) -> Result<BuildReport> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading the pack manifest at {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing the pack manifest at {}", manifest_path.display()))?;

    for tool in &manifest.metadata.tools {
        build_tool(dir, tool)?;
    }

    let publish_as = PublishAs {
        namespace: namespace.to_owned(),
    };
    let (bytes, artifact_digest) =
        pack_dir(dir, &publish_as).with_context(|| format!("packing {}", dir.display()))?;
    let packed =
        PackAfb::from_bytes(&bytes).context("reading back the pack that was just built")?;
    let pack_digest = packed
        .digest()
        .context("computing the pack's content digest")?;

    let out_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_out_path(dir, &manifest));
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&out_path, &bytes).with_context(|| format!("writing {}", out_path.display()))?;

    Ok(BuildReport {
        pack: manifest.name.clone(),
        version: manifest.version.clone(),
        artifact_digest,
        pack_digest,
        size_bytes: bytes.len(),
        out: out_path,
        tools: manifest
            .metadata
            .tools
            .iter()
            .map(|tool| ToolReport {
                name: tool.name.clone(),
                module: tool.module.clone(),
            })
            .collect(),
    })
}

fn default_out_path(dir: &Path, manifest: &PackManifest) -> PathBuf {
    let parent = dir.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}-{}.afb", manifest.name, manifest.version))
}

/// Makes sure one tool's module exists on disk before the pack is packed:
/// compiles it from `source` when the manifest names one, otherwise
/// requires it to already be there.
fn build_tool(dir: &Path, tool: &PackTool) -> Result<()> {
    let module_path = dir.join(&tool.module);
    let Some(source) = &tool.source else {
        anyhow::ensure!(
            module_path.is_file(),
            "tool {:?} has no source and its module is missing: {}",
            tool.name,
            module_path.display()
        );
        return Ok(());
    };
    let source_dir = dir.join(source);
    anyhow::ensure!(
        source_dir.join("Cargo.toml").is_file(),
        "tool {:?} declares source {source:?}, {}; building a tool from anything but a Rust \
         crate directory (one containing Cargo.toml) is not supported yet",
        tool.name,
        describe_source(&source_dir),
    );
    compile_rust_tool(&tool.name, &source_dir, &module_path)
}

fn describe_source(path: &Path) -> String {
    if !path.exists() {
        "which does not exist".to_owned()
    } else if path.is_file() {
        "which is a file, not a directory".to_owned()
    } else {
        "a directory with no Cargo.toml".to_owned()
    }
}

/// Builds a tool's Rust source with `cargo build --release --target
/// wasm32-wasip1`, then copies the produced module to `module_dest`.
fn compile_rust_tool(tool_name: &str, source_dir: &Path, module_dest: &Path) -> Result<()> {
    preflight_wasip1_target()?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest_path = source_dir.join("Cargo.toml");
    let status = std::process::Command::new(&cargo)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "--manifest-path",
        ])
        .arg(&manifest_path)
        .status()
        .with_context(|| format!("running cargo to build tool {tool_name:?}"))?;
    anyhow::ensure!(
        status.success(),
        "building tool {tool_name:?} failed; see the cargo output above"
    );
    let wasm = find_built_wasm(&manifest_path)
        .with_context(|| format!("locating the compiled module for tool {tool_name:?}"))?;
    if let Some(parent) = module_dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::copy(&wasm, module_dest).with_context(|| {
        format!(
            "copying the compiled module for tool {tool_name:?} to {}",
            module_dest.display()
        )
    })?;
    Ok(())
}

/// Verifies the `wasm32-wasip1` target is installed before a build is
/// attempted, so a missing target is a clear, actionable refusal rather
/// than a confusing `cargo build` failure.
fn preflight_wasip1_target() -> Result<()> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let output = std::process::Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .context("running `rustc --print sysroot`; install Rust from https://rustup.rs")?;
    anyhow::ensure!(
        output.status.success(),
        "`rustc --print sysroot` failed; check the Rust toolchain"
    );
    let sysroot = String::from_utf8_lossy(&output.stdout);
    let target_lib = Path::new(sysroot.trim()).join("lib/rustlib/wasm32-wasip1");
    anyhow::ensure!(
        target_lib.exists(),
        "the wasm32-wasip1 target is not installed; run: rustup target add wasm32-wasip1"
    );
    Ok(())
}

/// Finds the `.wasm` a `cargo build --target wasm32-wasip1` produced for
/// the crate at `manifest_path`. Uses `cargo metadata` for the actual
/// target directory rather than assuming `<source>/target`, since the
/// source crate may be nested inside this workspace and share its target
/// directory.
fn find_built_wasm(manifest_path: &Path) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = std::process::Command::new(&cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version=1",
            "--manifest-path",
        ])
        .arg(manifest_path)
        .output()
        .context("running cargo metadata")?;
    anyhow::ensure!(
        output.status.success(),
        "cargo metadata failed for {}",
        manifest_path.display()
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing cargo metadata output")?;
    let target_directory = metadata["target_directory"]
        .as_str()
        .context("cargo metadata carried no target_directory")?;
    let canonical_manifest = manifest_path
        .canonicalize()
        .unwrap_or_else(|_| manifest_path.to_path_buf());
    let package = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|package| {
            package["manifest_path"]
                .as_str()
                .map(PathBuf::from)
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path == canonical_manifest)
        })
        .context("cargo metadata did not describe this manifest")?;
    let package_name = package["name"].as_str().unwrap_or_default();

    let release_dir = Path::new(target_directory)
        .join("wasm32-wasip1")
        .join("release");
    let wasm_files: Vec<PathBuf> = std::fs::read_dir(&release_dir)
        .with_context(|| format!("reading {}", release_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "wasm")
                && path.parent() == Some(release_dir.as_path())
        })
        .collect();

    match wasm_files.as_slice() {
        [path] => Ok(path.clone()),
        [] => anyhow::bail!(
            "no .wasm was produced under {}; the crate needs a [[bin]] target",
            release_dir.display()
        ),
        paths => paths
            .iter()
            .find(|path| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| {
                        stem == package_name || stem.replace('-', "_") == package_name
                    })
            })
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} produced {} .wasm files and none match the crate name {package_name:?}; \
                     declare exactly one [[bin]] target",
                    release_dir.display(),
                    paths.len()
                )
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal tools-less pack (mirrors any of the 11 first-party
    /// packs' shape) so tests exercise the real manifest/asset contract
    /// instead of a hand-rolled fixture that could drift from it.
    fn write_plain_pack(root: &Path, name: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plain pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": name,
            "version": "1.0.0",
            "description": "A plain pack with no tools",
            "authors": ["gents-ai contributors"],
            "tags": ["example"],
            "kind": "assets",
            "assets": ["README.md"],
            "dependencies": [],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn write_tool_pack(root: &Path, name: &str, tool: serde_json::Value) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), b"# a tools pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": name,
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["tools"],
            "kind": "tools",
            "assets": ["README.md", tool["module"]],
            "tools": [tool],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_pack_with_no_tools_builds_and_packs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let out = dir.path().join("out.afb");
        let report = build_pack(&root, Some(&out), "gents").unwrap();

        assert_eq!(report.pack, "plain_pack");
        assert_eq!(report.version, "1.0.0");
        assert!(report.tools.is_empty());
        assert_eq!(report.out, out);
        assert!(out.is_file());
        assert_eq!(
            report.size_bytes,
            std::fs::metadata(&out).unwrap().len() as usize
        );

        let packed = PackAfb::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(packed.digest().unwrap(), report.pack_digest);
    }

    #[test]
    fn default_out_path_lands_beside_the_pack_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let report = build_pack(&root, None, "gents").unwrap();
        assert_eq!(report.out, dir.path().join("plain_pack-1.0.0.afb"));
        assert!(report.out.is_file());
    }

    #[test]
    fn a_tool_with_no_source_and_a_missing_module_names_both() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_tools");
        write_tool_pack(
            &root,
            "shipping_tools",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "module": "tools/format_check.wasm",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        // The module is declared but never written to disk.

        let error = build_pack(&root, None, "gents").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("tools/format_check.wasm"), "{message}");
        assert!(message.contains("missing"), "{message}");
    }

    #[test]
    fn a_tool_source_that_is_a_file_is_refused_as_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_tools");
        write_tool_pack(
            &root,
            "shipping_tools",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "module": "tools/format_check.wasm",
                "source": "tool_src",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::write(root.join("tool_src"), b"not a directory").unwrap();

        let error = build_pack(&root, None, "gents").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("is not supported yet"), "{message}");
        assert!(message.contains("file, not a directory"), "{message}");
    }

    #[test]
    fn a_tool_source_directory_without_cargo_toml_is_refused_as_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_tools");
        write_tool_pack(
            &root,
            "shipping_tools",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "module": "tools/format_check.wasm",
                "source": "tool_src",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::create_dir_all(root.join("tool_src")).unwrap();
        std::fs::write(root.join("tool_src/main.py"), b"print('hi')").unwrap();

        let error = build_pack(&root, None, "gents").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("is not supported yet"), "{message}");
        assert!(message.contains("no Cargo.toml"), "{message}");
    }

    #[test]
    fn missing_pack_dir_names_a_clear_error() {
        let error = build_pack(Path::new("/no/such/pack"), None, "gents").unwrap_err();
        assert!(format!("{error:#}").contains("manifest"));
    }

    #[test]
    fn build_all_builds_every_pack_directory_under_root() {
        let dir = tempfile::tempdir().unwrap();
        write_plain_pack(&dir.path().join("alpha"), "alpha");
        write_plain_pack(&dir.path().join("beta"), "beta");
        // A non-pack file beside the pack directories must not be treated
        // as one (mirrors packs/catalog.json sitting next to real packs).
        std::fs::write(dir.path().join("catalog.json"), b"[]").unwrap();

        let reports = build_all(dir.path(), "gents").unwrap();
        let mut names: Vec<_> = reports.iter().map(|report| report.pack.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
        for report in &reports {
            assert!(report.out.is_file());
        }
    }

    /// Compiles a real one-file Rust tool crate to `wasm32-wasip1` and
    /// packs it, exercising the actual `cargo build` + `cargo metadata`
    /// discovery path end to end rather than a stub.
    #[test]
    fn a_tool_with_a_rust_source_is_compiled_and_packed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_tools");
        write_tool_pack(
            &root,
            "shipping_tools",
            json!({
                "name": "format_check",
                "description": "Checks formatting and says what is wrong",
                "module": "tools/format_check.wasm",
                "source": "tool_src",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::create_dir_all(root.join("tool_src/src")).unwrap();
        std::fs::write(
            root.join("tool_src/Cargo.toml"),
            b"[package]\nname = \"format_check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
              [[bin]]\nname = \"format_check\"\npath = \"src/main.rs\"\n\n\
              [workspace]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tool_src/src/main.rs"),
            b"fn main() { println!(\"ok\"); }",
        )
        .unwrap();

        let report = build_pack(&root, None, "gents").unwrap();
        assert_eq!(report.tools.len(), 1);
        assert_eq!(report.tools[0].name, "format_check");

        let bytes = std::fs::read(&report.out).unwrap();
        let packed = PackAfb::from_bytes(&bytes).unwrap();
        let module = packed.tool_module("format_check").unwrap();
        assert!(module.starts_with(b"\0asm"), "compiled module is not wasm");
    }
}
