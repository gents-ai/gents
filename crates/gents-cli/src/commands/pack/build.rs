//! `gents pack build`: compile a pack's plugins and pack the result into
//! one `.tar.gz`, so first-party and third-party packs alike become the
//! artifact the registry serves.
//!
//! Compiling is this module's job; packing is [`gents::pack_archive::pack_dir`]'s.
//! A plugin with no `source` must already carry its compiled artifact. A
//! plugin with a `source` must be its own Afterburner package (a directory
//! with its own `afb.toml`), compiled through
//! [`crate::commands::afterburner_build`] - the one place this workspace
//! decides how an Afterburner package becomes a `.afb` and which languages
//! it will actually run bounded, shared with `gents plugin build` so the
//! two can never accept a language the other refuses. Everything else in
//! the manifest travels unchanged.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::pack::{PackManifest, PackPlugin};
use gents::pack_archive::{pack_dir, PackArchive};
use serde::Serialize;
use serde_json::json;

use crate::cli::args::PackBuildArgs;
use crate::commands::afterburner_build;

#[derive(Debug, Serialize)]
pub(crate) struct BuildReport {
    pub(crate) pack: String,
    /// The coordinate's other half, so what `publish` will place it under
    /// is visible at build time rather than discovered at publish time.
    pub(crate) namespace: String,
    pub(crate) version: String,
    pub(crate) artifact_digest: String,
    pub(crate) pack_digest: String,
    pub(crate) size_bytes: usize,
    pub(crate) out: PathBuf,
    pub(crate) plugins: Vec<PluginReport>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginReport {
    pub(crate) name: String,
    pub(crate) artifact: String,
}

pub(crate) fn dispatch(args: PackBuildArgs) -> Result<()> {
    if args.all {
        let reports = build_all(Path::new("packs"))?;
        return crate::print_json(&json!({ "packs": reports }));
    }
    let dir = args.dir.as_deref().context(
        "PACK_DIR is required; pass a directory or --all to build every pack under packs/",
    )?;
    let report = build_pack(dir, args.out.as_deref())?;
    crate::print_json(&serde_json::to_value(report)?)
}

/// Builds every pack directory under `root` (any directory that carries a
/// `manifest.json`) into a `.tar.gz` beside it, so this repository's own
/// packs can be published to the registry in one step.
pub(crate) fn build_all(root: &Path) -> Result<Vec<BuildReport>> {
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
        .map(|dir| build_pack(dir, None).with_context(|| format!("building {}", dir.display())))
        .collect()
}

/// Compiles `dir`'s declared plugins onto disk, then packs the whole
/// directory into a `.tar.gz` at `out` (or the default sibling path).
pub(crate) fn build_pack(dir: &Path, out: Option<&Path>) -> Result<BuildReport> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading the pack manifest at {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing the pack manifest at {}", manifest_path.display()))?;

    for plugin in &manifest.metadata.plugins {
        build_plugin(dir, plugin)?;
    }

    let (bytes, artifact_digest) =
        pack_dir(dir).with_context(|| format!("packing {}", dir.display()))?;
    let packed =
        PackArchive::from_bytes(&bytes).context("reading back the pack that was just built")?;
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
        namespace: manifest.metadata.namespace.clone(),
        version: manifest.version.clone(),
        artifact_digest,
        pack_digest,
        size_bytes: bytes.len(),
        out: out_path,
        plugins: manifest
            .metadata
            .plugins
            .iter()
            .map(|plugin| PluginReport {
                name: plugin.name.clone(),
                artifact: plugin.artifact.clone(),
            })
            .collect(),
    })
}

fn default_out_path(dir: &Path, manifest: &PackManifest) -> PathBuf {
    let parent = dir.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}-{}.tar.gz", manifest.name, manifest.version))
}

/// Makes sure one plugin's artifact exists on disk before the pack is
/// packed: compiles it from `source` through
/// [`crate::commands::afterburner_build`] when the manifest names one,
/// otherwise requires it to already be there.
fn build_plugin(dir: &Path, plugin: &PackPlugin) -> Result<()> {
    let artifact_path = dir.join(&plugin.artifact);
    let Some(source) = &plugin.source else {
        anyhow::ensure!(
            artifact_path.is_file(),
            "plugin {:?} has no source and its artifact is missing: {}",
            plugin.name,
            artifact_path.display()
        );
        return Ok(());
    };
    let source_dir = dir.join(source);
    let owner = format!("plugin {:?}", plugin.name);
    let local = afterburner_build::load_and_validate(&owner, &source_dir)?;
    afterburner_build::compile(&owner, &source_dir, local, &artifact_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal plugin-less pack (mirrors any of the 11 first-party
    /// packs' shape) so tests exercise the real manifest/asset contract
    /// instead of a hand-rolled fixture that could drift from it.
    fn write_plain_pack(root: &Path, name: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plain pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": name,
            "version": "1.0.0",
            "description": "A plain pack with no plugins",
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

    fn write_plugin_pack(root: &Path, name: &str, plugin: serde_json::Value) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plugins pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": name,
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["plugins"],
            "kind": "plugins",
            "assets": ["README.md", plugin["artifact"]],
            "plugins": [plugin],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_pack_with_no_plugins_builds_and_packs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let out = dir.path().join("out.tar.gz");
        let report = build_pack(&root, Some(&out)).unwrap();

        assert_eq!(report.pack, "plain_pack");
        assert_eq!(report.version, "1.0.0");
        assert!(report.plugins.is_empty());
        assert_eq!(report.out, out);
        assert!(out.is_file());
        assert_eq!(
            report.size_bytes,
            std::fs::metadata(&out).unwrap().len() as usize
        );

        let packed = PackArchive::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(packed.digest().unwrap(), report.pack_digest);
    }

    #[test]
    fn default_out_path_lands_beside_the_pack_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let report = build_pack(&root, None).unwrap();
        assert_eq!(report.out, dir.path().join("plain_pack-1.0.0.tar.gz"));
        assert!(report.out.is_file());
    }

    #[test]
    fn a_plugin_with_no_source_and_a_missing_artifact_names_both() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        // The artifact is declared but never written to disk.

        let error = build_pack(&root, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("plugins/format_check.afb"), "{message}");
        assert!(message.contains("missing"), "{message}");
    }

    #[test]
    fn a_plugin_source_that_is_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "source": "plugin_src",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::write(root.join("plugin_src"), b"not a directory").unwrap();

        let error = build_pack(&root, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("own Afterburner package"), "{message}");
        assert!(message.contains("file, not a directory"), "{message}");
    }

    #[test]
    fn a_plugin_source_directory_without_afb_toml_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "source": "plugin_src",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::create_dir_all(root.join("plugin_src")).unwrap();
        std::fs::write(root.join("plugin_src/main.rs"), b"fn main() {}").unwrap();

        let error = build_pack(&root, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("own Afterburner package"), "{message}");
        assert!(message.contains("no afb.toml"), "{message}");
    }

    /// The pack manifest's own `PackPlugin::validate` (exercised end to
    /// end here through `build_pack`, which packs the manifest once every
    /// declared plugin's artifact is on disk) is what actually rejects an
    /// unsupported language; `crate::pack`'s own unit tests check that
    /// rule directly. This proves `gents pack build` surfaces the same
    /// refusal, naming the plugin and the language it declared.
    #[test]
    fn a_plugin_with_an_unknown_language_is_refused_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "language": "haskell",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        // No source: the artifact only needs to exist on disk for
        // `build_plugin` to pass, so the refusal below is purely about the
        // language.
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(root.join("plugins/format_check.afb"), b"stub").unwrap();

        let error = build_pack(&root, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("haskell"), "{message}");
    }

    #[test]
    fn missing_pack_dir_names_a_clear_error() {
        let error = build_pack(Path::new("/no/such/pack"), None).unwrap_err();
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

        let reports = build_all(dir.path()).unwrap();
        let mut names: Vec<_> = reports.iter().map(|report| report.pack.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
        for report in &reports {
            assert!(report.out.is_file());
        }
    }

    /// Compiles a real Rust Afterburner package to `wasm32-wasip1` through
    /// [`crate::commands::afterburner_build`] and packs it, exercising the
    /// actual `pkg::LocalPackage::load` + native-toolchain compile path end
    /// to end rather than a stub. Every language `gents pack build` can
    /// compile a plugin from is exercised directly, once, in
    /// `afterburner_build`'s own tests; this proves the pack-manifest
    /// integration around that shared compiler, not the compiler itself
    /// again per language.
    #[test]
    fn a_plugin_with_a_rust_source_is_compiled_through_afterburner_and_packed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting and says what is wrong",
                "artifact": "plugins/format_check.afb",
                "source": "plugin_src",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        let plugin_src = root.join("plugin_src");
        std::fs::create_dir_all(plugin_src.join("source")).unwrap();
        std::fs::write(
            plugin_src.join("afb.toml"),
            b"[format]\nversion = \"1.0\"\n\n\
              [package]\nname = \"format_check\"\nnamespace = \"gents\"\nversion = \"0.1.0\"\n\
              language = \"rust\"\nentry = \"source/main.rs\"\n\n\
              [runtime]\nmin = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            plugin_src.join("manifold.json"),
            br#"{"fs":"None","net":"None","env":"None","crypto":false,"child_process":false}"#,
        )
        .unwrap();
        std::fs::write(
            plugin_src.join("Cargo.toml"),
            b"[workspace]\n\n\
              [package]\nname = \"format_check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
              [[bin]]\nname = \"format_check\"\npath = \"source/main.rs\"\n",
        )
        .unwrap();
        std::fs::write(
            plugin_src.join("source/main.rs"),
            b"fn main() { println!(\"ok\"); }",
        )
        .unwrap();

        let report = build_pack(&root, None).unwrap();
        assert_eq!(report.plugins.len(), 1);
        assert_eq!(report.plugins[0].name, "format_check");
        assert_eq!(report.plugins[0].artifact, "plugins/format_check.afb");

        let bytes = std::fs::read(&report.out).unwrap();
        let packed = PackArchive::from_bytes(&bytes).unwrap();
        let artifact_bytes = packed.plugin_artifact("format_check").unwrap();
        let artifact_afb = afterburner_cloud::Afb::from_bytes(artifact_bytes)
            .expect("the compiled plugin artifact is a real .afb");
        assert_eq!(artifact_afb.manifest.package.language, "rust");
        assert_eq!(
            artifact_afb.manifest.runtime.target.as_deref(),
            Some("wasm32-wasip1"),
            "a Rust plugin compiles to a wasm32-wasip1 WASI command module"
        );
    }
}
