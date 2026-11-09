//! A plugin's source held on the node, turned into the pack directory the
//! build owner compiles. A `PluginSource` document is one entry file plus its
//! cases; this writes them where `build_plugin` and `run_plugin_cases`
//! already look, through the scaffold owner, so a plugin built from a
//! document and one built from a directory share the compiler, the cases
//! runner and the manifest shape.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::scaffold::{scaffold, ScaffoldInput};
use super::template::Template;
use crate::documents::{PluginSourceDocument, PLUGIN_SOURCE_NAMESPACE};
use crate::pack::resolve::{ResolvedFrom, ResolvedNamedPack};
use crate::pack::{PackPlugin, PLUGIN_ARTIFACT_PREFIX};

/// The build's sandbox root for the plugin `plugin_id` under `home`; the pack
/// directory is `<root>/<plugin_id>`.
pub fn plugin_source_build_root(home: &Path, plugin_id: &str) -> PathBuf {
    home.join(crate::store::local::home::PLUGIN_BUILDS_DIR_NAME)
        .join(plugin_id)
}

/// The manifest entry the build owner compiles for `document`: sealed (no
/// manifold), with the document's schema and data declaration.
pub fn manifest_entry(document: &PluginSourceDocument) -> PackPlugin {
    let name = &document.plugin_id;
    PackPlugin {
        name: name.clone(),
        description: document
            .description
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| format!("{name} tool.")),
        artifact: format!("{PLUGIN_ARTIFACT_PREFIX}{name}.afb"),
        source: Some(format!("{PLUGIN_ARTIFACT_PREFIX}{name}")),
        language: document.language.trim().to_ascii_lowercase(),
        input_schema: document
            .input_schema
            .clone()
            .unwrap_or_else(|| json!({"type": "object"})),
        data: document.data.clone(),
        ..PackPlugin::default()
    }
}

/// One source document laid out for a build.
#[derive(Clone, Debug)]
pub struct PreparedPluginSource {
    /// The build's sandbox root.
    pub root: PathBuf,
    /// The one-plugin pack directory inside it.
    pub dir: PathBuf,
    /// The pack's version: the installed build's patch number plus one.
    pub version: String,
    /// Whether an earlier build of this plugin is installed.
    pub installed: bool,
}

/// The version of the next build, so each install is a distinct, ordered
/// version of the one-plugin pack.
fn next_version(installed: Option<&str>) -> String {
    let patch = installed
        .and_then(|version| version.rsplit('.').next())
        .and_then(|patch| patch.parse::<u64>().ok())
        .map_or(0, |patch| patch + 1);
    format!("0.1.{patch}")
}

/// Lays `document` out under `home` for the build owner: refuses what the
/// manifest would, picks the next version and writes the pack directory.
pub fn prepare_plugin_source(
    home: &Path,
    document: &PluginSourceDocument,
) -> Result<PreparedPluginSource> {
    let installed = crate::pack::read_home_install(home, &document.coordinate())?;
    let version = next_version(installed.as_ref().map(|record| record.version.as_str()));
    let root = plugin_source_build_root(home, &document.plugin_id);
    let dir = root.join(&document.plugin_id);
    materialize_plugin_source(&dir, document, &version)?;
    Ok(PreparedPluginSource {
        root,
        dir,
        version,
        installed: installed.is_some(),
    })
}

/// The built pack directory, imported into the home's pack store and opened
/// for the plugins-pack installer.
pub fn open_built_plugin_source(
    home: &Path,
    prepared: &PreparedPluginSource,
) -> Result<ResolvedNamedPack> {
    let archive =
        crate::pack::local::open(&crate::pack::local::LocalName::Path(&prepared.dir), home)?;
    Ok(ResolvedNamedPack {
        archive,
        from: ResolvedFrom::Store,
    })
}

/// Writes `document` as a one-plugin pack at `dir`, replacing what an earlier
/// build left except the compiler's `target` cache. `version` is the pack's
/// version; the caller raises it per installed build.
pub fn materialize_plugin_source(
    dir: &Path,
    document: &PluginSourceDocument,
    version: &str,
) -> Result<()> {
    document.validate()?;
    let plugin = manifest_entry(document);
    plugin.validate()?;
    let name = plugin.name.as_str();
    if dir.exists() {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            if entry.file_name() == "target" {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
            .with_context(|| format!("clearing {}", path.display()))?;
        }
    }
    scaffold(
        dir,
        name,
        &ScaffoldInput {
            namespace: PLUGIN_SOURCE_NAMESPACE.to_owned(),
            template: Some(Template::PluginTool),
            kind: None,
            language: Some(plugin.language.clone()),
            ux: false,
        },
    )?;
    let source_dir = dir.join(plugin.source.as_deref().context("plugin source path")?);
    let entry = super::build::plugin_entry(&plugin.language)
        .with_context(|| format!("gents cannot build {:?} plugins", plugin.language))?;
    std::fs::write(source_dir.join(entry), &document.source)
        .with_context(|| format!("writing {entry}"))?;
    let cases_dir = source_dir.join("tests");
    std::fs::remove_dir_all(&cases_dir).context("clearing the scaffolded cases")?;
    std::fs::create_dir_all(&cases_dir).context("creating the cases directory")?;
    for (index, case) in document.cases().iter().enumerate() {
        let label = case
            .name
            .as_deref()
            .map(super::names::to_snake_case)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "case".to_owned());
        let mut body = json!({ "input": case.input });
        if let Some(expect) = &case.expect {
            body["expect"] = expect.clone();
        }
        std::fs::write(
            cases_dir.join(format!("{:02}_{label}.json", index + 1)),
            serde_json::to_vec_pretty(&body)?,
        )
        .with_context(|| format!("writing case {}", index + 1))?;
    }
    let manifest_path = dir.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).context("reading the scaffolded manifest")?,
    )
    .context("parsing the scaffolded manifest")?;
    let scaffolded = manifest["plugins"][0].take();
    let mut entry = serde_json::to_value(&plugin)?;
    entry["instructions"] = scaffolded["instructions"].clone();
    manifest["plugins"] = json!([entry]);
    manifest["version"] = json!(version);
    manifest["description"] = json!(plugin.description);
    let assets = manifest["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter(|asset| {
                    asset
                        .as_str()
                        .is_some_and(|path| !path.ends_with("/tests/echo.json"))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    manifest["assets"] = json!(assets);
    if let Some(instructions) = scaffolded["instructions"].as_str() {
        std::fs::write(
            dir.join(instructions),
            format!("# {name}\n\n{}\n", plugin.description),
        )
        .with_context(|| format!("writing {instructions}"))?;
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)? + "\n",
    )
    .context("writing manifest.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(value: Value) -> PluginSourceDocument {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_source_document_becomes_the_pack_the_build_owner_compiles() {
        let home = tempfile::tempdir().unwrap();
        let root = plugin_source_build_root(home.path(), "double");
        let dir = root.join("double");
        let source = document(json!({
            "plugin_id": "double", "node_did": "did:key:n", "language": "js",
            "description": "Doubles n.",
            "source": "module.exports = (input) => ({ n: input.n * 2 });",
            "input_schema": {"type": "object", "properties": {"n": {"type": "number"}}},
            "tests": [{"name": "Two", "input": {"n": 2}, "expect": {"n": 4}}, {"input": {"n": 0}}],
        }));
        std::fs::create_dir_all(dir.join("target/plugins")).unwrap();
        std::fs::write(dir.join("stale.txt"), "old").unwrap();
        materialize_plugin_source(&dir, &source, "0.1.3").unwrap();

        assert!(!dir.join("stale.txt").exists());
        assert!(
            dir.join("target/plugins").is_dir(),
            "the compiler cache stays"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("plugins/double/source/main.js")).unwrap(),
            source.source
        );
        let cases: Vec<_> = std::fs::read_dir(dir.join("plugins/double/tests"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(cases, ["01_two.json", "02_case.json"]);
        let manifest = crate::pack::authoring::read_manifest(&dir).unwrap();
        assert_eq!(manifest.metadata.namespace, "node");
        assert_eq!(manifest.version, "0.1.3");
        assert_eq!(manifest.metadata.kind, crate::pack::PackKind::Plugins);
        let plugin = &manifest.metadata.plugins[0];
        assert_eq!(plugin.source.as_deref(), Some("plugins/double"));
        assert_eq!(plugin.input_schema["properties"]["n"]["type"], "number");
        assert_eq!(plugin.description, "Doubles n.");
        assert_eq!(
            crate::pack::authoring::plugins_with_source(&manifest),
            ["double"]
        );

        materialize_plugin_source(&dir, &source, "0.1.4").unwrap();
        assert_eq!(
            crate::pack::authoring::read_manifest(&dir).unwrap().version,
            "0.1.4"
        );

        let prepared = prepare_plugin_source(home.path(), &source).unwrap();
        assert_eq!(prepared.dir, dir);
        assert_eq!(prepared.version, "0.1.0", "nothing is installed yet");
        assert!(!prepared.installed);
    }

    #[test]
    fn the_manifest_rules_refuse_a_source_before_anything_is_written() {
        let home = tempfile::tempdir().unwrap();
        let cobol = document(json!({
            "plugin_id": "ledger", "node_did": "did:key:n", "language": "cobol", "source": "x",
        }));
        let error = format!(
            "{:#}",
            prepare_plugin_source(home.path(), &cobol).unwrap_err()
        );
        assert!(error.contains("supported languages"), "{error}");
        assert!(!plugin_source_build_root(home.path(), "ledger").exists());
    }

    #[test]
    fn each_installed_build_gets_the_next_patch_version() {
        assert_eq!(next_version(None), "0.1.0");
        assert_eq!(next_version(Some("0.1.0")), "0.1.1");
        assert_eq!(next_version(Some("0.1.41")), "0.1.42");
        assert_eq!(next_version(Some("weird")), "0.1.0");
    }
}
