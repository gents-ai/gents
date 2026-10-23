use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;

use crate::pack::{PackManifest, PackPlugin};

pub trait PluginCompiler {
    fn builder_id(&self) -> &str {
        BUILDER_ID
    }

    fn load_and_validate(&self, owner: &str, dir: &Path) -> Result<()>;
    fn compile(&self, owner: &str, dir: &Path, out_path: &Path) -> Result<()>;
}

#[derive(Debug, Serialize)]
pub struct BuildReport {
    pub pack: String,
    pub namespace: String,
    pub version: String,
    pub digest: String,
    pub size_bytes: u64,
    pub out: PathBuf,
    pub plugins: Vec<PluginReport>,
}

#[derive(Debug, Serialize)]
pub struct PluginReport {
    pub name: String,
    pub artifact: String,
}

pub fn build_all(
    root: &Path,
    compiler: &dyn PluginCompiler,
    out: impl Fn(&Path) -> Result<Option<PathBuf>>,
) -> Result<Vec<BuildReport>> {
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
            build_pack(dir, compiler, out(dir)?.as_deref())
                .with_context(|| format!("building {}", dir.display()))
        })
        .collect()
}

pub fn build_pack(
    dir: &Path,
    compiler: &dyn PluginCompiler,
    out: Option<&Path>,
) -> Result<BuildReport> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading the pack manifest at {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing the pack manifest at {}", manifest_path.display()))?;

    for plugin in &manifest.metadata.plugins {
        build_plugin(dir, &manifest, plugin, compiler)?;
    }
    // A file-produced UX plugin the webview would refuse never gets packed;
    // an .afb-produced one is linted when its producer runs.
    let refused: Vec<String> = crate::ux_plugin::lint_pack_dir(dir, &manifest.metadata.ux)?
        .into_iter()
        .filter(|(_, findings)| !findings.is_empty())
        .map(|(name, findings)| {
            format!(
                "ux plugin {name:?}: {}",
                crate::ux_plugin::lint::describe(&findings)
            )
        })
        .collect();
    anyhow::ensure!(
        refused.is_empty(),
        "the static lint refuses the pack's ux plugins:\n{}",
        refused.join("\n")
    );

    let out_dir = match out.and_then(Path::parent) {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) => PathBuf::from("."),
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
        let header = crate::pack_archive::write_pack(dir, &mut writer)
            .with_context(|| format!("packing {}", dir.display()))?;
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

pub fn build_plugin(
    dir: &Path,
    manifest: &PackManifest,
    plugin: &PackPlugin,
    compiler: &dyn PluginCompiler,
) -> Result<()> {
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

    let stamp = staged.join("target").join("gents-build.stamp");
    let source_digest = format!("{} {}", compiler.builder_id(), tree_digest(&staged)?);
    if let (Ok(recorded), Ok(artifact)) = (
        std::fs::read_to_string(&stamp),
        std::fs::read(&artifact_path),
    ) {
        if recorded == format!("{source_digest} {}", sha256_hex(&artifact)) {
            return Ok(());
        }
    }
    compiler.load_and_validate(&owner, &staged)?;
    compiler.compile(&owner, &staged, &artifact_path)?;
    let artifact = std::fs::read(&artifact_path)
        .with_context(|| format!("reading {}", artifact_path.display()))?;
    std::fs::create_dir_all(stamp.parent().context("stamp parent")?)?;
    std::fs::write(&stamp, format!("{source_digest} {}", sha256_hex(&artifact)))
        .with_context(|| format!("writing {}", stamp.display()))
}

pub const BUILDER_ID: &str = concat!("gents/", env!("CARGO_PKG_VERSION"));

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

pub fn plugin_entry(language: &str) -> Option<&'static str> {
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

    #[derive(Clone)]
    struct RefusingCompiler {
        called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl RefusingCompiler {
        fn new() -> (Self, std::sync::Arc<std::sync::atomic::AtomicBool>) {
            let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            (
                Self {
                    called: called.clone(),
                },
                called,
            )
        }
    }

    impl PluginCompiler for RefusingCompiler {
        fn load_and_validate(&self, _owner: &str, _dir: &Path) -> Result<()> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("no compiler in this test")
        }
        fn compile(&self, _owner: &str, _dir: &Path, _out_path: &Path) -> Result<()> {
            unreachable!("load_and_validate refuses first")
        }
    }

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
        let (compiler, _called) = RefusingCompiler::new();

        let out = dir.path().join("out.pack");
        let report = build_pack(&root, &compiler, Some(&out)).unwrap();

        assert_eq!(report.pack, "plain_pack");
        assert_eq!(report.version, "1.0.0");
        assert!(report.plugins.is_empty());
        assert_eq!(report.out, out);
        assert!(out.is_file());
        assert_eq!(report.size_bytes, std::fs::metadata(&out).unwrap().len());

        let packed =
            crate::pack_archive::PackArchive::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
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
        let (compiler, _) = RefusingCompiler::new();

        let report = build_pack(&root, &compiler, None).unwrap();
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
        let (compiler, called) = RefusingCompiler::new();

        let error = build_pack(&root, &compiler, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("plugins/format_check.afb"), "{message}");
        assert!(message.contains("missing"), "{message}");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "a missing artifact is refused before the compiler seam is asked"
        );
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
        let (compiler, called) = RefusingCompiler::new();

        let error = build_pack(&root, &compiler, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("is not a directory"), "{message}");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "a malformed source is refused before the compiler seam is asked"
        );
    }

    #[test]
    fn a_stamp_recorded_by_this_builder_skips_the_seam_and_a_foreign_stamp_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        let (compiler, called) = RefusingCompiler::new();
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
        std::fs::create_dir_all(root.join("plugin_src/source")).unwrap();
        std::fs::write(
            root.join("plugin_src/Cargo.toml"),
            b"[workspace]\n\n[package]\nname = \"format_check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"format_check\"\npath = \"source/main.rs\"\n",
        )
        .unwrap();
        std::fs::write(root.join("plugin_src/source/main.rs"), b"fn main() {}\n").unwrap();
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(root.join("plugins/format_check.afb"), b"stub artifact").unwrap();

        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let staged = root.join("target/plugins/format_check");
        mirror(&root.join("plugin_src"), &staged).unwrap();
        write_if_changed(
            &staged.join("afb.toml"),
            format!(
                "[format]\nversion = \"1.0\"\n\n[package]\nname = {}\nnamespace = {}\nversion = {}\n\
                 language = {}\nentry = {}\n\n[runtime]\nmin = \"0.1.0\"\n",
                toml_string("format_check"),
                toml_string(&manifest.metadata.namespace),
                toml_string(&manifest.version),
                toml_string("rust"),
                toml_string("source/main.rs"),
            )
            .as_bytes(),
        )
        .unwrap();
        let plugin = &manifest.metadata.plugins[0];
        let manifold = plugin.manifold.clone().unwrap_or_else(|| {
            json!({"fs": "None", "net": "None", "env": "None", "crypto": false, "child_process": false})
        });
        write_if_changed(
            &staged.join("manifold.json"),
            &serde_json::to_vec(&manifold).unwrap(),
        )
        .unwrap();
        let source_digest = format!("{BUILDER_ID} {}", tree_digest(&staged).unwrap());
        let artifact = std::fs::read(root.join("plugins/format_check.afb")).unwrap();
        std::fs::create_dir_all(staged.join("target")).unwrap();
        std::fs::write(
            staged.join("target/gents-build.stamp"),
            format!("{source_digest} {}", sha256_hex(&artifact)),
        )
        .unwrap();

        build_pack(&root, &compiler, None).unwrap();
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "unchanged source with this builder's stamp skips the compiler"
        );

        let recorded = std::fs::read_to_string(staged.join("target/gents-build.stamp")).unwrap();
        std::fs::write(
            staged.join("target/gents-build.stamp"),
            recorded.replacen(BUILDER_ID, "gents-cli/0.0.0-other", 1),
        )
        .unwrap();
        let error = build_pack(&root, &compiler, None).unwrap_err();
        assert!(
            format!("{error:#}").contains("no compiler in this test"),
            "{error:#}"
        );
        assert!(
            called.load(std::sync::atomic::Ordering::SeqCst),
            "a foreign stamp reaches the compiler seam"
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
        let (compiler, _) = RefusingCompiler::new();

        let error = build_pack(&root, &compiler, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("has no source/main.rs"), "{message}");
    }

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
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(root.join("plugins/format_check.afb"), b"stub").unwrap();
        let (compiler, _) = RefusingCompiler::new();

        let error = build_pack(&root, &compiler, None).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("haskell"), "{message}");
    }

    #[test]
    fn missing_pack_dir_names_a_clear_error() {
        let (compiler, _) = RefusingCompiler::new();
        let error = build_pack(Path::new("/no/such/pack"), &compiler, None).unwrap_err();
        assert!(format!("{error:#}").contains("manifest"));
    }

    #[test]
    fn build_all_builds_every_pack_directory_under_root() {
        let dir = tempfile::tempdir().unwrap();
        write_plain_pack(&dir.path().join("alpha"), "alpha");
        write_plain_pack(&dir.path().join("beta"), "beta");
        std::fs::write(dir.path().join("catalog.json"), b"[]").unwrap();
        let (compiler, _) = RefusingCompiler::new();

        let reports = build_all(dir.path(), &compiler, |_| Ok(None)).unwrap();
        let mut names: Vec<_> = reports.iter().map(|report| report.pack.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
        for report in &reports {
            assert!(report.out.is_file());
        }
    }

    #[test]
    fn every_supported_language_names_an_entry() {
        for language in crate::pack::SUPPORTED_PLUGIN_LANGUAGES {
            assert!(plugin_entry(language).is_some(), "{language} has no entry");
        }
        assert_eq!(plugin_entry("rust"), Some("source/main.rs"));
        assert_eq!(plugin_entry("Rust "), Some("source/main.rs"));
        assert_eq!(plugin_entry("haskell"), None);
    }
}
