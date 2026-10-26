use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::pack::{PackManifest, PackPlugin};

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("manifest.json has no assets list")]
    AssetsKeyMissing,
    #[error("manifest.json assets is not a list")]
    AssetsNotList,
    #[error("manifest.json assets list is not closed")]
    AssetsNotClosed,
    #[error("walked outside the plugin")]
    OutsidePlugin,
    #[error("no pack directories with a manifest.json were found under {}", root.display())]
    NoPackDirectories { root: PathBuf },
    #[error("creating {}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading {path}")]
    ReadAsset {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("removing {}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading the pack manifest at {}", path.display())]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing the pack manifest at {}", path.display())]
    ParseManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("resolving {}", path.display())]
    Resolve {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("staging the pack in {}", path.display())]
    StageDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("packing {}", path.display())]
    Pack {
        path: PathBuf,
        #[source]
        source: crate::pack_archive::format::PackFormatError,
    },
    #[error("writing the pack")]
    FlushPack(#[source] std::io::Error),
    #[error("sizing the pack")]
    SizePack(#[source] std::io::Error),
    #[error("the static lint refuses the pack's ux plugins:\n{findings}")]
    UxRefused { findings: String },
    #[error(transparent)]
    Ux(#[from] crate::ux_plugin::UxPluginError),
    #[error("plan path has no parent")]
    PlanPathParent,
    #[error("reading manifest.json")]
    ReadManifestJson(#[source] std::io::Error),
    #[error("writing manifest.json")]
    WriteManifestJson(#[source] std::io::Error),
    #[error("plugin {name:?} has no source and its artifact is missing: {}", path.display())]
    ArtifactMissing { name: String, path: PathBuf },
    #[error("{owner} declares language {language:?}, which gents cannot build")]
    UnbuildableLanguage { owner: String, language: String },
    #[error("{owner} declares source {declared:?}, which is not a directory")]
    SourceNotDirectory { owner: String, declared: String },
    #[error("{owner} has no {entry} in {}", dir.display())]
    EntryMissing {
        owner: String,
        entry: &'static str,
        dir: PathBuf,
    },
    #[error("stamp parent")]
    StampParent,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Compiler(crate::pack::CallbackError),
    #[error(transparent)]
    Out(crate::pack::CallbackError),
    #[error(transparent)]
    GraphPackage(#[from] crate::graph_package::GraphPackageError),
    #[error("building {}", dir.display())]
    Building {
        dir: PathBuf,
        #[source]
        source: Box<BuildError>,
    },
}

type Result<T, E = BuildError> = std::result::Result<T, E>;

pub trait PluginCompiler {
    fn builder_id(&self) -> &str {
        BUILDER_ID
    }

    fn load_and_validate(&self, owner: &str, dir: &Path)
        -> Result<(), crate::pack::CallbackError>;
    fn compile(
        &self,
        owner: &str,
        dir: &Path,
        out_path: &Path,
    ) -> Result<(), crate::pack::CallbackError>;
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

pub fn build_all<O>(
    root: &Path,
    compiler: &dyn PluginCompiler,
    out: impl Fn(&Path) -> Result<Option<PathBuf>, O>,
) -> Result<Vec<BuildReport>>
where
    O: Into<crate::pack::CallbackError>,
{
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|source| BuildError::Read {
            path: root.to_path_buf(),
            source,
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("manifest.json").is_file())
        .collect();
    dirs.sort();
    if dirs.is_empty() {
        return Err(BuildError::NoPackDirectories {
            root: root.to_path_buf(),
        });
    }
    dirs.iter()
        .map(|dir| {
            let out = out(dir).map_err(|error| BuildError::Out(error.into()))?;
            build_pack(dir, compiler, out.as_deref()).map_err(|source| BuildError::Building {
                dir: dir.clone(),
                source: Box::new(source),
            })
        })
        .collect()
}

pub fn build_pack(
    dir: &Path,
    compiler: &dyn PluginCompiler,
    out: Option<&Path>,
) -> Result<BuildReport> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes =
        std::fs::read(&manifest_path).map_err(|source| BuildError::ReadManifest {
            path: manifest_path.clone(),
            source,
        })?;
    let manifest: PackManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|source| BuildError::ParseManifest {
            path: manifest_path.clone(),
            source,
        })?;

    for plugin in &manifest.metadata.plugins {
        build_plugin(dir, &manifest, plugin, compiler)?;
    }
    if manifest.metadata.kind == crate::pack::PackKind::Graph {
        build_graph_plans(dir, &manifest)?;
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
    if !refused.is_empty() {
        return Err(BuildError::UxRefused {
            findings: refused.join("\n"),
        });
    }

    let out_dir = match out.and_then(Path::parent) {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) => PathBuf::from("."),
        None => std::path::absolute(dir)
            .map_err(|source| BuildError::Resolve {
                path: dir.to_path_buf(),
                source,
            })?
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    };
    std::fs::create_dir_all(&out_dir).map_err(|source| BuildError::Create {
        path: out_dir.clone(),
        source,
    })?;
    let mut staged = tempfile::Builder::new()
        .prefix(".building-")
        .tempfile_in(&out_dir)
        .map_err(|source| BuildError::StageDir {
            path: out_dir.clone(),
            source,
        })?;
    let header = {
        let mut writer = std::io::BufWriter::new(staged.as_file_mut());
        let header = crate::pack_archive::write_pack(dir, &mut writer).map_err(|source| {
            BuildError::Pack {
                path: dir.to_path_buf(),
                source,
            }
        })?;
        std::io::Write::flush(&mut writer).map_err(BuildError::FlushPack)?;
        header
    };
    let size_bytes = staged
        .as_file()
        .metadata()
        .map_err(BuildError::SizePack)?
        .len();
    let out_path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| out_dir.join(header.file_name()));
    staged
        .persist(&out_path)
        .map_err(|error| BuildError::Write {
            path: out_path.clone(),
            source: error.error,
        })?;

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

pub fn build_graph_plans(dir: &Path, manifest: &PackManifest) -> Result<()> {
    let plans = crate::graph_package::compile_pack_graphs(
        manifest,
        &|path: &str| -> Result<Vec<u8>> {
            std::fs::read(dir.join(path)).map_err(|source| BuildError::ReadAsset {
                path: path.to_owned(),
                source,
            })
        },
        "did:key:zPackBuildPlaceholderOwner",
    )?;
    let mut missing = Vec::new();
    for plan in &plans {
        let path = crate::graph_package::graph_plan_path(&plan.graph_id);
        let target = dir.join(&path);
        std::fs::create_dir_all(target.parent().ok_or(BuildError::PlanPathParent)?)?;
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
    let text = std::fs::read_to_string(&manifest_path).map_err(BuildError::ReadManifestJson)?;
    let mut assets = manifest.metadata.assets.clone();
    assets.extend(missing);
    Ok(
        std::fs::write(&manifest_path, replace_assets(&text, &assets)?)
            .map_err(BuildError::WriteManifestJson)?,
    )
}

fn replace_assets(text: &str, assets: &[String]) -> Result<String> {
    let key = text
        .find("\"assets\"")
        .ok_or(BuildError::AssetsKeyMissing)?;
    let open = key + text[key..].find('[').ok_or(BuildError::AssetsNotList)?;
    let close = open + text[open..].find(']').ok_or(BuildError::AssetsNotClosed)?;
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

pub fn build_plugin(
    dir: &Path,
    manifest: &PackManifest,
    plugin: &PackPlugin,
    compiler: &dyn PluginCompiler,
) -> Result<()> {
    let artifact_path = dir.join(&plugin.artifact);
    let Some(source) = &plugin.source else {
        if !artifact_path.is_file() {
            return Err(BuildError::ArtifactMissing {
                name: plugin.name.clone(),
                path: artifact_path,
            });
        }
        return Ok(());
    };
    let source_dir = dir.join(source);
    let owner = format!("plugin {:?}", plugin.name);
    let entry = plugin_entry(&plugin.language).ok_or_else(|| BuildError::UnbuildableLanguage {
        owner: owner.clone(),
        language: plugin.language.clone(),
    })?;
    if !source_dir.is_dir() {
        return Err(BuildError::SourceNotDirectory {
            owner,
            declared: source.clone(),
        });
    }
    if !source_dir.join(entry).is_file() {
        return Err(BuildError::EntryMissing {
            owner,
            entry,
            dir: source_dir,
        });
    }
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
    compiler
        .load_and_validate(&owner, &staged)
        .map_err(BuildError::Compiler)?;
    compiler
        .compile(&owner, &staged, &artifact_path)
        .map_err(BuildError::Compiler)?;
    let artifact = std::fs::read(&artifact_path).map_err(|source| BuildError::Read {
        path: artifact_path.clone(),
        source,
    })?;
    std::fs::create_dir_all(stamp.parent().ok_or(BuildError::StampParent)?)?;
    Ok(
        std::fs::write(&stamp, format!("{source_digest} {}", sha256_hex(&artifact))).map_err(
            |source| BuildError::Write {
                path: stamp.clone(),
                source,
            },
        )?,
    )
}

pub const BUILDER_ID: &str = concat!("gents/", env!("CARGO_PKG_VERSION"));

fn tree_digest(dir: &Path) -> Result<String> {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
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
    walk(dir, dir, &mut files).map_err(|source| BuildError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    files.sort();
    let mut hasher = sha2::Sha256::new();
    for file in files {
        let relative = file
            .strip_prefix(dir)
            .map_err(|_| BuildError::OutsidePlugin)?;
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
    std::fs::create_dir_all(to).map_err(|source| BuildError::Create {
        path: to.to_path_buf(),
        source,
    })?;
    let mut kept = std::collections::BTreeSet::new();
    let entries = std::fs::read_dir(from).map_err(|source| BuildError::Read {
        path: from.to_path_buf(),
        source,
    })?;
    for entry in entries {
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
    let entries = std::fs::read_dir(to).map_err(|source| BuildError::Read {
        path: to.to_path_buf(),
        source,
    })?;
    for entry in entries {
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
        .map_err(|source| BuildError::Remove {
            path: path.clone(),
            source,
        })?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    std::fs::write(path, bytes).map_err(|source| BuildError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::error_chain::ErrorChain;

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
        fn load_and_validate(
            &self,
            _owner: &str,
            _dir: &Path,
        ) -> std::result::Result<(), crate::pack::CallbackError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Err("no compiler in this test".into())
        }
        fn compile(
            &self,
            _owner: &str,
            _dir: &Path,
            _out_path: &Path,
        ) -> std::result::Result<(), crate::pack::CallbackError> {
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
        let message = format!("{}", ErrorChain(&error));
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
        let message = format!("{}", ErrorChain(&error));
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("is not a directory"), "{message}");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "a malformed source is refused before the compiler seam is asked"
        );
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
            format!("{}", ErrorChain(&error)).contains("no compiler in this test"),
            "{}",
            ErrorChain(&error)
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
        let message = format!("{}", ErrorChain(&error));
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
        let message = format!("{}", ErrorChain(&error));
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("haskell"), "{message}");
    }

    #[test]
    fn missing_pack_dir_names_a_clear_error() {
        let (compiler, _) = RefusingCompiler::new();
        let error = build_pack(Path::new("/no/such/pack"), &compiler, None).unwrap_err();
        assert!(format!("{}", ErrorChain(&error)).contains("manifest"));
    }

    #[test]
    fn build_all_builds_every_pack_directory_under_root() {
        let dir = tempfile::tempdir().unwrap();
        write_plain_pack(&dir.path().join("alpha"), "alpha");
        write_plain_pack(&dir.path().join("beta"), "beta");
        std::fs::write(dir.path().join("catalog.json"), b"[]").unwrap();
        let (compiler, _) = RefusingCompiler::new();

        let reports = build_all(dir.path(), &compiler, |_| Ok::<_, BuildError>(None)).unwrap();
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
