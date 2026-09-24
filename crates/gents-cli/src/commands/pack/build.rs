//! `gents pack build`: compile a pack's plugins and pack the result into
//! one `.pack` file, so first-party and third-party packs alike become the
//! artifact the registry serves.
//!
//! Compiling is this module's job; packing is [`gents::pack_archive::write_pack`]'s.
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
use gents::pack_archive::write_pack;
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
    /// The pack digest: what installs, the store and the registry name it by.
    pub(crate) digest: String,
    pub(crate) size_bytes: u64,
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
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let report = build_pack(&dir, args.out.as_deref())?;
    crate::print_json(&serde_json::to_value(report)?)
}

/// Builds every pack directory under `root` (any directory that carries a
/// `manifest.json`) into a `.pack` beside it, so this repository's own
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
/// directory into a `.pack` at `out`, or beside `dir` under the pack's file
/// name. The file is written to a staging name and renamed into place, so an
/// interrupted build never leaves a partial `.pack` behind.
pub(crate) fn build_pack(dir: &Path, out: Option<&Path>) -> Result<BuildReport> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading the pack manifest at {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing the pack manifest at {}", manifest_path.display()))?;

    for plugin in &manifest.metadata.plugins {
        build_plugin(dir, &manifest, plugin)?;
    }
    if manifest.metadata.kind == gents::pack::PackKind::Graph {
        build_graph_plans(dir, &manifest)?;
    }

    let out_dir = match out.and_then(Path::parent) {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) => PathBuf::from("."),
        // Beside the pack directory, even when it was named as `.`.
        None => std::path::absolute(dir)
            .with_context(|| format!("resolving {}", dir.display()))?
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    };
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".building-")
        .tempfile_in(&out_dir)
        .with_context(|| format!("staging the pack in {}", out_dir.display()))?;
    let header = {
        let mut writer = std::io::BufWriter::new(staged.as_file_mut());
        let header =
            write_pack(dir, &mut writer).with_context(|| format!("packing {}", dir.display()))?;
        std::io::Write::flush(&mut writer).context("writing the pack")?;
        header
    };
    let size_bytes = staged
        .as_file()
        .metadata()
        .context("sizing the pack")?
        .len();
    let out_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out_dir.join(header.file_name()));
    staged
        .persist(&out_path)
        .map_err(|error| error.error)
        .with_context(|| format!("writing {}", out_path.display()))?;

    Ok(BuildReport {
        pack: manifest.name.clone(),
        namespace: manifest.metadata.namespace.clone(),
        version: manifest.version.clone(),
        digest: header.digest,
        size_bytes,
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

/// Compiles every graph the pack declares and writes each plan to
/// `graphs/<graph_id>.plan.json`, declaring it in `manifest.json` when it is
/// not yet, so the pack digest covers the plan install will verify.
pub(crate) fn build_graph_plans(dir: &Path, manifest: &PackManifest) -> Result<()> {
    let plans = gents::graph_package::compile_pack_graphs(
        manifest,
        &|path| std::fs::read(dir.join(path)).with_context(|| format!("reading {path}")),
        "did:key:zPackBuildPlaceholderOwner",
    )?;
    let mut missing = Vec::new();
    for plan in &plans {
        let path = gents::graph_package::graph_plan_path(&plan.graph_id);
        let target = dir.join(&path);
        std::fs::create_dir_all(target.parent().context("plan path has no parent")?)?;
        write_if_changed(
            &target,
            (serde_json::to_string_pretty(plan)? + "\n").as_bytes(),
        )?;
        if !manifest.metadata.assets.contains(&path) {
            missing.push(path);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).context("reading manifest.json")?;
    // Appended: the author's order stays, and the digest sorts anyway.
    let mut assets = manifest.metadata.assets.clone();
    assets.extend(missing);
    std::fs::write(&manifest_path, replace_assets(&text, &assets)?).context("writing manifest.json")
}

/// `text` with its top-level `"assets"` list replaced by `assets`, one per
/// line at the list's own indentation; nothing else in the file moves.
fn replace_assets(text: &str, assets: &[String]) -> Result<String> {
    let key = text
        .find("\"assets\"")
        .context("manifest.json has no assets list")?;
    let open = key
        + text[key..]
            .find('[')
            .context("manifest.json assets is not a list")?;
    let close = open
        + text[open..]
            .find(']')
            .context("manifest.json assets list is not closed")?;
    let line_start = text[..key].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..key];
    let items = assets
        .iter()
        .map(|asset| format!("{indent}  {}", serde_json::Value::from(asset.as_str())))
        .collect::<Vec<_>>()
        .join(",\n");
    Ok(format!(
        "{}[\n{items}\n{indent}]{}",
        &text[..open],
        &text[close + 1..]
    ))
}

/// Makes sure one plugin's artifact exists on disk before the pack is
/// packed: compiles it from `source` when the manifest names one, otherwise
/// requires it to already be there.
///
/// A plugin's source is plain source in its language (see [`plugin_entry`]).
/// The build copies it to `<pack>/target/plugins/<name>/`, writes the package
/// description the compiler reads there from the manifest entry, and compiles
/// the copy, so the author's directory holds only their code and a rebuild
/// reuses the language toolchain's own cache in the staged copy.
fn build_plugin(dir: &Path, manifest: &PackManifest, plugin: &PackPlugin) -> Result<()> {
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
    let entry = plugin_entry(&plugin.language).with_context(|| {
        format!(
            "{owner} declares language {:?}, which gents cannot build",
            plugin.language
        )
    })?;
    anyhow::ensure!(
        source_dir.is_dir(),
        "{owner} declares source {source:?}, which is not a directory"
    );
    anyhow::ensure!(
        source_dir.join(entry).is_file(),
        "{owner} has no {entry} in {}",
        source_dir.display()
    );
    let staged = dir.join("target").join("plugins").join(&plugin.name);
    mirror(&source_dir, &staged)?;
    write_if_changed(
        &staged.join("afb.toml"),
        format!(
            "[format]\nversion = \"1.0\"\n\n[package]\nname = {}\nnamespace = {}\nversion = {}\n\
             language = {}\nentry = {}\n\n[runtime]\nmin = \"0.1.0\"\n",
            toml_string(&plugin.name),
            toml_string(&manifest.metadata.namespace),
            toml_string(&manifest.version),
            toml_string(&plugin.language),
            toml_string(entry),
        )
        .as_bytes(),
    )?;
    let manifold = plugin.manifold.clone().unwrap_or_else(|| {
        json!({"fs": "None", "net": "None", "env": "None", "crypto": false, "child_process": false})
    });
    write_if_changed(
        &staged.join("manifold.json"),
        &serde_json::to_vec(&manifold)?,
    )?;

    // Unchanged source, compiler and the artifact this build last produced
    // from them: skip. The compiler is linked into this binary, so its
    // version is part of the key; an upgrade may change `.afb` output.
    let stamp = staged.join("target").join("gents-build.stamp");
    let source_digest = format!("{BUILDER_ID} {}", tree_digest(&staged)?);
    if let (Ok(recorded), Ok(artifact)) = (
        std::fs::read_to_string(&stamp),
        std::fs::read(&artifact_path),
    ) {
        if recorded == format!("{source_digest} {}", sha256_hex(&artifact)) {
            return Ok(());
        }
    }
    let local = afterburner_build::load_and_validate(&owner, &staged)?;
    afterburner_build::compile(&owner, &staged, local, &artifact_path)?;
    let artifact = std::fs::read(&artifact_path)
        .with_context(|| format!("reading {}", artifact_path.display()))?;
    std::fs::create_dir_all(stamp.parent().context("stamp parent")?)?;
    std::fs::write(&stamp, format!("{source_digest} {}", sha256_hex(&artifact)))
        .with_context(|| format!("writing {}", stamp.display()))
}

const BUILDER_ID: &str = concat!("gents-cli/", env!("CARGO_PKG_VERSION"));

/// Digest of every file under `dir` except the toolchain's `target/`, by
/// relative path and content, in path order.
fn tree_digest(dir: &Path) -> Result<String> {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_name() == "target" && dir == root {
                continue;
            }
            if entry.file_type()?.is_dir() {
                walk(root, &entry.path(), files)?;
            } else {
                files.push(entry.path());
            }
        }
        Ok(())
    }
    use sha2::Digest;
    let mut files = Vec::new();
    walk(dir, dir, &mut files).with_context(|| format!("reading {}", dir.display()))?;
    files.sort();
    let mut hasher = sha2::Sha256::new();
    for file in files {
        let relative = file
            .strip_prefix(dir)
            .context("walked outside the plugin")?;
        let bytes = std::fs::read(&file)?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The entry file a plugin's source holds, by language: the one convention
/// the scaffolder writes and the build compiles.
pub(crate) fn plugin_entry(language: &str) -> Option<&'static str> {
    Some(match language.trim().to_ascii_lowercase().as_str() {
        "rust" => "source/main.rs",
        "go" | "golang" => "source/main.go",
        "c" => "source/main.c",
        "cpp" | "c++" | "cxx" | "cc" => "source/main.cpp",
        "js" | "javascript" => "source/main.js",
        "ts" | "typescript" => "source/main.ts",
        "python" | "py" => "source/main.py",
        "ruby" | "rb" => "source/main.rb",
        _ => return None,
    })
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Makes `to` a copy of `from`, rewriting only files whose bytes changed so a
/// toolchain's incremental cache in `to` stays valid, and removing what
/// `from` no longer has. The staged `target/` and the generated package
/// description are the build's own and are kept.
fn mirror(from: &Path, to: &Path) -> Result<()> {
    mirror_level(from, to, true)
}

fn mirror_level(from: &Path, to: &Path, top: bool) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let mut kept = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        let (source, target) = (entry.path(), to.join(&name));
        if entry.file_type()?.is_dir() {
            mirror_level(&source, &target, false)?;
        } else {
            write_if_changed(&target, &std::fs::read(&source)?)?;
        }
        kept.insert(name);
    }
    for entry in std::fs::read_dir(to).with_context(|| format!("reading {}", to.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let generated =
            top && matches!(name.to_str(), Some("target" | "afb.toml" | "manifold.json"));
        if generated || kept.contains(&name) {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::pack_archive::PackArchive;

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

        let out = dir.path().join("out.pack");
        let report = build_pack(&root, Some(&out)).unwrap();

        assert_eq!(report.pack, "plain_pack");
        assert_eq!(report.version, "1.0.0");
        assert!(report.plugins.is_empty());
        assert_eq!(report.out, out);
        assert!(out.is_file());
        assert_eq!(report.size_bytes, std::fs::metadata(&out).unwrap().len());

        let packed = PackArchive::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(packed.digest(), report.digest);
        let staging_left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".building-")
            })
            .collect();
        assert!(
            staging_left.is_empty(),
            "the staging file is renamed, not left"
        );
    }

    #[test]
    fn default_out_path_lands_beside_the_pack_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let report = build_pack(&root, None).unwrap();
        assert_eq!(report.out, dir.path().join("gents.plain_pack-1.0.0.pack"));
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
        assert!(message.contains("is not a directory"), "{message}");
    }

    #[test]
    fn declaring_a_plan_touches_only_the_assets_list() {
        let text = "{\n  \"name\": \"x\",\n  \"assets\": [\n    \"README.md\"\n  ],\n  \"kind\": \"graph\"\n}\n";
        let updated =
            replace_assets(text, &["README.md".into(), "graphs/x.plan.json".into()]).unwrap();
        assert_eq!(
            updated,
            "{\n  \"name\": \"x\",\n  \"assets\": [\n    \"README.md\",\n    \"graphs/x.plan.json\"\n  ],\n  \"kind\": \"graph\"\n}\n"
        );
    }

    /// A second build of unchanged source reuses the artifact; a source edit
    /// rebuilds it.
    #[test]
    fn an_unchanged_plugin_is_not_recompiled() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("format_check");
        super::super::scaffold::scaffold(
            &dir,
            "format_check",
            &crate::cli::PackScaffoldArgs {
                kind: None,
                namespace: "acme".into(),
                template: Some(crate::cli::PackTemplate::PluginTool),
                language: None,
            },
        )
        .unwrap();
        let _guard = crate::commands::afterburner_build::compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let artifact = dir.join("plugins/format_check.afb");
        build_pack(&dir, Some(&root.path().join("a.pack"))).unwrap();
        let first = std::fs::metadata(&artifact).unwrap().modified().unwrap();
        build_pack(&dir, Some(&root.path().join("b.pack"))).unwrap();
        assert_eq!(
            std::fs::metadata(&artifact).unwrap().modified().unwrap(),
            first,
            "unchanged source must not be recompiled"
        );

        let stamp = dir.join("target/plugins/format_check/target/gents-build.stamp");
        let recorded = std::fs::read_to_string(&stamp).unwrap();
        let other_builder = recorded.replacen(super::BUILDER_ID, "gents-cli/0.0.0-other", 1);
        assert_ne!(recorded, other_builder, "the stamp records the builder");
        std::fs::write(&stamp, other_builder).unwrap();
        build_pack(&dir, Some(&root.path().join("b2.pack"))).unwrap();
        let rebuilt = std::fs::metadata(&artifact).unwrap().modified().unwrap();
        assert_ne!(rebuilt, first, "another compiler's artifact is rebuilt");
        let first = rebuilt;

        let source = dir.join("plugins/format_check/source/main.rs");
        let edited = std::fs::read_to_string(&source).unwrap() + "\n// edited\n";
        std::fs::write(&source, edited).unwrap();
        build_pack(&dir, Some(&root.path().join("c.pack"))).unwrap();
        assert_ne!(
            std::fs::metadata(&artifact).unwrap().modified().unwrap(),
            first,
            "an edited source is rebuilt"
        );
    }

    #[test]
    fn a_plugin_source_without_its_entry_file_is_refused() {
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
        assert!(message.contains("has no source/main.rs"), "{message}");
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
