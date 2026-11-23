use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents_fs_runner::beneath::{ChildKind, Intent, Located, OpenRoot, RootDir, Walk};
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
    /// The pack's UX plugins, by name.
    pub ux: Vec<String>,
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
    let held = RootDir::open(root)?;
    let dirs: Vec<_> = held
        .entries()?
        .into_iter()
        .filter(|entry| {
            entry.kind == ChildKind::Directory
                && held.is_file(Path::new(&entry.name).join("manifest.json"))
        })
        .collect();
    anyhow::ensure!(
        !dirs.is_empty(),
        "no pack directories with a manifest.json were found under {}",
        root.display()
    );
    dirs.iter()
        .map(|entry| {
            let dir = root.join(&entry.name);
            build_pack(&held.subdir(&entry.name)?, compiler, out(&dir)?.as_deref())
                .with_context(|| format!("building {}", dir.display()))
        })
        .collect()
}

pub fn build_pack(
    dir: impl OpenRoot,
    compiler: &dyn PluginCompiler,
    out: Option<&Path>,
) -> Result<BuildReport> {
    let dir = dir
        .open_root()
        .context("opening the pack directory that holds manifest.json")?;
    let manifest_path = dir.path().join("manifest.json");
    let manifest_bytes = dir
        .read("manifest.json")
        .with_context(|| format!("reading the pack manifest at {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing the pack manifest at {}", manifest_path.display()))?;

    for plugin in &manifest.metadata.plugins {
        build_plugin(&dir, &manifest, plugin, compiler)?;
    }
    let out = out_path(&dir, &manifest, out)?;
    pack_directory(&dir, &manifest, &Located::at(&out, Intent::Create)?, out)
}

/// Packs a directory whose plugins are already artifacts: prebuilt ones, or
/// ones a compiler just produced. Without `out` the pack lands beside the
/// directory.
pub fn pack_built(dir: impl OpenRoot, out: Option<&Located>) -> Result<BuildReport> {
    let dir = dir.open_root()?;
    let manifest = super::check::read_manifest(&dir)?;
    match out {
        Some(out) => pack_directory(&dir, &manifest, out, out.path()),
        None => {
            let out = out_path(&dir, &manifest, None)?;
            pack_directory(&dir, &manifest, &Located::at(&out, Intent::Create)?, out)
        }
    }
}

/// A pack's plugins with `source`, which only a compiler turns into artifacts.
pub fn plugins_with_source(manifest: &PackManifest) -> Vec<&str> {
    manifest
        .metadata
        .plugins
        .iter()
        .filter(|plugin| plugin.source.is_some())
        .map(|plugin| plugin.name.as_str())
        .collect()
}

/// Where `pack_built` writes the pack of `dir` when given no output path.
pub fn built_pack_path(dir: &Path, manifest: &PackManifest) -> PathBuf {
    dir.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(built_pack_name(manifest))
}

/// The file name a build gives the pack of `manifest`.
pub fn built_pack_name(manifest: &PackManifest) -> String {
    crate::pack::archive::format::pack_file_name(
        &format!("{}/{}", manifest.metadata.namespace, manifest.name),
        &manifest.version,
    )
}

fn out_path(dir: &RootDir, manifest: &PackManifest, out: Option<&Path>) -> Result<PathBuf> {
    Ok(match out {
        Some(out) => out.to_path_buf(),
        None => built_pack_path(dir.path(), manifest),
    })
}

/// Packs `dir` into `out` through a staged sibling renamed over it in the
/// directory `out` was resolved in; `shown` is the path the report names.
fn pack_directory(
    dir: &RootDir,
    manifest: &PackManifest,
    out: &Located,
    shown: PathBuf,
) -> Result<BuildReport> {
    // A file-produced UX plugin the webview would refuse never gets packed;
    // an .afb-produced one is linted when its producer runs.
    let refused: Vec<String> = crate::plugin::ux::lint_pack_dir(dir, &manifest.metadata.ux)?
        .into_iter()
        .filter(|(_, findings)| !findings.is_empty())
        .map(|(name, findings)| {
            format!(
                "ux plugin {name:?}: {}",
                crate::plugin::ux::lint::describe(&findings)
            )
        })
        .collect();
    anyhow::ensure!(
        refused.is_empty(),
        "the static lint refuses the pack's ux plugins:\n{}",
        refused.join("\n")
    );

    let mut packed = None;
    out.replace_with(|file| -> Result<()> {
        let mut writer = std::io::BufWriter::new(&mut *file);
        let header = crate::pack::archive::write_built_pack(
            dir,
            &mut writer,
            &crate::pack::core_packs::runtime_release(),
        )
        .with_context(|| format!("packing {}", dir.path().display()))?;
        std::io::Write::flush(&mut writer).context("writing the pack")?;
        drop(writer);
        let size_bytes = file.metadata().context("sizing the pack")?.len();
        packed = Some((header, size_bytes));
        Ok(())
    })
    .with_context(|| format!("writing {}", shown.display()))?;
    let (header, size_bytes) = packed.context("the pack was not written")?;

    Ok(BuildReport {
        pack: manifest.name.clone(),
        namespace: manifest.metadata.namespace.clone(),
        version: manifest.version.clone(),
        digest: header.digest,
        size_bytes,
        out: shown,
        plugins: manifest
            .metadata
            .plugins
            .iter()
            .map(|plugin| PluginReport {
                name: plugin.name.clone(),
                artifact: plugin.artifact.clone(),
            })
            .collect(),
        ux: manifest
            .metadata
            .ux
            .iter()
            .map(|page| page.name.clone())
            .collect(),
    })
}

/// Compiles one plugin's source into its artifact. The source is mirrored
/// through held handles into `target/plugins/<name>`, the compiler runs on
/// that staged directory by its path, and its artifact is read back through
/// the pack's handle.
pub fn build_plugin(
    dir: impl OpenRoot,
    manifest: &PackManifest,
    plugin: &PackPlugin,
    compiler: &dyn PluginCompiler,
) -> Result<()> {
    let dir = &dir.open_root()?;
    let artifact_path = dir.path().join(&plugin.artifact);
    let Some(source) = &plugin.source else {
        anyhow::ensure!(
            dir.is_file(&plugin.artifact),
            "plugin {:?} has no source and its artifact is missing: {}",
            plugin.name,
            artifact_path.display()
        );
        return Ok(());
    };
    let owner = format!("plugin {:?}", plugin.name);
    let entry = plugin_entry(&plugin.language).with_context(|| {
        format!(
            "{owner} declares language {:?}, which gents cannot build",
            plugin.language
        )
    })?;
    anyhow::ensure!(
        dir.is_dir(source),
        "{owner} declares source {source:?}, which is not a directory"
    );
    let source_dir = dir.subdir(source)?;
    anyhow::ensure!(
        source_dir.is_file(entry),
        "{owner} has no {entry} in {}",
        source_dir.path().display()
    );
    let staged = dir.create_dir_all(Path::new("target").join("plugins").join(&plugin.name))?;
    mirror(&source_dir, &staged)?;
    write_if_changed(
        &staged,
        "afb.toml",
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
    write_if_changed(&staged, "manifold.json", &serde_json::to_vec(&manifold)?)?;

    let stamp = Path::new("target").join("gents-build.stamp");
    let source_digest = format!("{} {}", compiler.builder_id(), tree_digest(&staged)?);
    if let (Ok(recorded), Ok(artifact)) = (
        staged.read_to_string(&stamp),
        dir.read(&plugin.artifact),
    ) {
        if recorded == format!("{source_digest} {}", sha256_hex(&artifact)) {
            return Ok(());
        }
    }
    compiler.load_and_validate(&owner, staged.path())?;
    compiler.compile(&owner, staged.path(), &artifact_path)?;
    let artifact = dir
        .read(&plugin.artifact)
        .with_context(|| format!("reading {}", artifact_path.display()))?;
    staged
        .write(&stamp, format!("{source_digest} {}", sha256_hex(&artifact)))
        .with_context(|| format!("writing {}", staged.path().join(&stamp).display()))
}

pub const BUILDER_ID: &str = concat!("gents/", env!("CARGO_PKG_VERSION"));

fn tree_digest(dir: &RootDir) -> Result<String> {
    use sha2::Digest;
    let mut files = Vec::new();
    dir.walk(|entry| {
        if entry.relative() == Path::new("target") {
            return Ok(Walk::Skip);
        }
        if entry.kind() != ChildKind::Directory {
            files.push((entry.relative().to_path_buf(), entry.read()?));
        }
        Ok(Walk::Enter)
    })
    .with_context(|| format!("reading {}", dir.path().display()))?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = sha2::Sha256::new();
    for (relative, bytes) in files {
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

fn mirror(from: &RootDir, to: &RootDir) -> Result<()> {
    mirror_level(from, to, true)
}

fn mirror_level(from: &RootDir, to: &RootDir, top: bool) -> Result<()> {
    let mut kept = std::collections::BTreeSet::new();
    for entry in from
        .entries()
        .with_context(|| format!("reading {}", from.path().display()))?
    {
        let name = entry.name;
        if name == "target" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        if entry.kind == ChildKind::Directory {
            mirror_level(&from.subdir(&name)?, &to.create_dir_all(&name)?, false)?;
        } else {
            write_if_changed(to, &name, &from.read(&name)?)?;
        }
        kept.insert(name);
    }
    for entry in to
        .entries()
        .with_context(|| format!("reading {}", to.path().display()))?
    {
        let name = entry.name;
        let generated =
            top && matches!(name.to_str(), Some("target" | "afb.toml" | "manifold.json"));
        if generated || kept.contains(&name) {
            continue;
        }
        to.remove(&name)
            .with_context(|| format!("removing {}", to.path().join(&name).display()))?;
    }
    Ok(())
}

fn write_if_changed(dir: &RootDir, name: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    let name = name.as_ref();
    if dir.read(name).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    dir.write(name, bytes)
        .with_context(|| format!("writing {}", dir.path().join(name).display()))
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
            crate::pack::archive::PackArchive::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(packed.digest(), report.digest);
        assert_eq!(
            packed.manifest().gents_version,
            Some(crate::pack::core_packs::runtime_release().to_string()),
            "the build writes its own release into a manifest that declares none"
        );
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
        assert_eq!(
            report.out,
            std::fs::canonicalize(dir.path())
                .unwrap()
                .join("gents.plain_pack-1.0.0.pack")
        );
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
        let held = RootDir::open(&root).unwrap();
        let staged_dir = held.create_dir_all("target/plugins/format_check").unwrap();
        mirror(&held.subdir("plugin_src").unwrap(), &staged_dir).unwrap();
        write_if_changed(
            &staged_dir,
            "afb.toml",
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
            &staged_dir,
            "manifold.json",
            &serde_json::to_vec(&manifold).unwrap(),
        )
        .unwrap();
        let source_digest = format!("{BUILDER_ID} {}", tree_digest(&staged_dir).unwrap());
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
