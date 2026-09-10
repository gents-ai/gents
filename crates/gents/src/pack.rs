//! Distribution/catalog boundary. Execution and writes remain owned by the
//! graph installer and desired-state installer, not by package resolution.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[path = "pack_asset_path.rs"]
mod asset_path;

include!(concat!(env!("OUT_DIR"), "/bundled_packs.rs"));

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    Graph,
    Documents,
    Assets,
    /// Nothing but capabilities: complete Afterburner `.afb` plugins a
    /// graph stage or a model can call. A plugins pack installs no
    /// documents of its own, and its plugins are callable by any pack in
    /// the same home, which is what lets one pack build on another's
    /// capabilities instead of vendoring them.
    Plugins,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMetadata {
    pub kind: PackKind,
    /// The registry namespace this pack publishes under.
    ///
    /// Absent means [`crate::pack_archive::DEFAULT_NAMESPACE`], so a
    /// first-party pack does not repeat it while a third-party pack can
    /// name its own. The registry reads this same field out of the same
    /// `manifest.json`; a pack that could be built here and refused there
    /// for want of a namespace is the divergence this field closes.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    pub authors: Vec<String>,
    pub tags: Vec<String>,
    pub assets: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// The capabilities this pack builds and ships.
    ///
    /// A plugin is a complete, sandboxed Afterburner `.afb`. Naming it
    /// here is what `gents pack build` compiles and what `gents pack
    /// install` places in the plugin store, from where `gents plugin run`
    /// calls it by name. Offering the same admitted plugin to a graph
    /// stage and to a model as a tool is the point of one definition, and
    /// neither of those two call paths is wired yet: today a plugin is
    /// built, shipped, installed, and called directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PackPlugin>,
}

fn default_namespace() -> String {
    crate::pack_archive::DEFAULT_NAMESPACE.to_owned()
}

/// One capability a pack ships.
///
/// The artifact is a complete Afterburner `.afb`, built from the source the
/// pack carries, addressed by its path inside the pack, and admitted by
/// digest at install time. The schema is what a model is shown; the
/// manifold is what the pack asks the sandbox to allow, which an
/// operator's ceiling can narrow and never widen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackPlugin {
    /// What a graph stage or a model calls it by, unique within the pack.
    pub name: String,
    /// What it does, in the words a model is given.
    pub description: String,
    /// The compiled `.afb` inside the pack, under [`PLUGIN_ARTIFACT_PREFIX`]
    /// by convention (`plugins/<name>.afb`). Always a full `.afb`, never a
    /// bare `.wasm`: that is the one representation Afterburner's compiler
    /// can emit for every language it supports, since a Python plugin
    /// compiles to an emscripten-pyodide bundle rather than a WASI command
    /// and a bare `.wasm` cannot carry that. Only Afterburner itself
    /// knows how to dispatch every one of those shapes, which is why a
    /// plugin runs on Afterburner's own runtime rather than a bespoke one
    /// here (see `crate::plugin`).
    pub artifact: String,
    /// Where the artifact is built from, relative to the pack root.
    /// Present for a pack that carries its sources, absent for one that
    /// ships only the compiled artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The source language `gents pack build` compiles `source` with, one
    /// of the identifiers `afterburner::cli::compile::lang::SourceLang`
    /// accepts (`rust`, `go`, `c`, `cpp`, `js`, ... see
    /// [`SUPPORTED_PLUGIN_LANGUAGES`] for the exact list). Required even
    /// for a plugin that ships
    /// only a compiled artifact, so a pack's manifest always says what
    /// built it.
    pub language: String,
    /// JSON Schema for the arguments, and what the model is shown.
    pub input_schema: serde_json::Value,
    /// What the plugin asks the sandbox for. Absent means it asks for
    /// nothing, which is the right default for a pure transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifold: Option<serde_json::Value>,
}

/// Where a plugin's compiled artifact must live inside a pack, by
/// convention. Checked in [`PackPlugin::validate`] so a pack author learns
/// a misplaced artifact at the pack rather than at install.
pub const PLUGIN_ARTIFACT_PREFIX: &str = "plugins/";

/// Languages `gents pack build` can compile a plugin's `source` from.
///
/// Mirrors exactly what `afterburner::cli::compile::lang::SourceLang::from_str`
/// accepts. That type lives behind the `afterburner` crate's `bin` feature,
/// which is a dependency of `gents-cli` (where the actual compiling
/// happens) and not of this crate, so this is a plain, independent copy
/// rather than a shared import.
///
/// This list says what `gents pack build` knows how to compile, and
/// nothing more. Whether a *built* artifact can then be run with every
/// bound it declares actually enforced is a separate question, answered
/// against the compiled `.afb` itself by `crate::plugin`'s runner (which
/// asks `afterburner::afb_run::bounds_for` rather than deciding for
/// itself). Keeping the two apart matters: a language can compile to more
/// than one shape - Ruby compiles to an ordinary WASI command, which is
/// fully bounded, while a hand-built Ruby-source `.afb` is not - so a
/// language name alone cannot answer it.
///
/// vertexia: kept in sync by hand with afterburner's own list; the ceiling
/// is a shared, lightweight language-id crate both sides could depend on if
/// this ever drifts.
pub const SUPPORTED_PLUGIN_LANGUAGES: &[&str] = &[
    "js",
    "javascript",
    "ts",
    "typescript",
    "rust",
    "go",
    "golang",
    "c",
    "cpp",
    "c++",
    "cxx",
    "cc",
    "python",
    "py",
    "ruby",
    "rb",
];

impl PackPlugin {
    /// The rules a plugin has to satisfy before anything will build or
    /// admit it. Checked when a pack is resolved, so a malformed plugin is
    /// a refusal at the pack rather than a surprise at the call.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            is_valid_pack_name(&self.name),
            "plugin name must be snake_case: {:?}",
            self.name
        );
        anyhow::ensure!(
            !self.description.trim().is_empty(),
            "plugin {:?} needs a description; it is what the model is shown",
            self.name
        );
        anyhow::ensure!(
            self.artifact.ends_with(".afb"),
            "plugin {:?} artifact must be a compiled .afb path, got {:?}",
            self.name,
            self.artifact
        );
        anyhow::ensure!(
            is_distributable_asset_path(&self.artifact),
            "unsafe plugin artifact path: {:?}",
            self.artifact
        );
        anyhow::ensure!(
            self.artifact.starts_with(PLUGIN_ARTIFACT_PREFIX),
            "plugin {:?} artifact must live under {PLUGIN_ARTIFACT_PREFIX}, got {:?}",
            self.name,
            self.artifact
        );
        let language = self.language.trim().to_ascii_lowercase();
        anyhow::ensure!(
            SUPPORTED_PLUGIN_LANGUAGES.contains(&language.as_str()),
            "plugin {:?} declares language {:?}, which is not one gents pack build knows how to \
             compile; supported languages: {}",
            self.name,
            self.language,
            SUPPORTED_PLUGIN_LANGUAGES.join(", ")
        );
        if let Some(source) = &self.source {
            anyhow::ensure!(
                is_distributable_asset_path(source),
                "unsafe plugin source path: {source:?}"
            );
        }
        anyhow::ensure!(
            self.input_schema.is_object(),
            "plugin {:?} needs an object input_schema",
            self.name
        );
        if let Some(manifold) = &self.manifold {
            // A plugin is called, never a server. Asking to bind a port is
            // refused at the pack rather than silently dropped later, so a
            // pack author learns it here instead of wondering why it never
            // worked.
            let listen = manifold.get("listen");
            anyhow::ensure!(
                listen.is_none_or(|value| value == "None" || value == &serde_json::json!("None")),
                "plugin {:?} asks to listen on a port; a pack's plugins are called, not served",
                self.name
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(flatten)]
    pub metadata: PackMetadata,
    // Graph-specific fields are validated by the existing graph loader.
    #[serde(flatten)]
    pub graph: BTreeMap<String, serde_json::Value>,
}

pub struct ResolvedPack {
    pub manifest: PackManifest,
    pub digest: String,
}

/// Return whether `name` is admissible at the pack catalog and source-pack
/// boundaries. Keep callers on this owner instead of growing parallel name
/// validators in adapters.
pub fn is_valid_pack_name(name: &str) -> bool {
    asset_path::is_snake_case_name(name)
}

impl ResolvedPack {
    pub fn asset(&self, path: &str) -> Result<&'static [u8]> {
        anyhow::ensure!(
            path == "manifest.json" || self.manifest.metadata.assets.iter().any(|p| p == path),
            "undeclared pack asset: {path}"
        );
        bundled_pack_asset(&self.manifest.name, path).context("missing bundled pack asset")
    }
}

/// Whether a path may appear in a pack archive.
///
/// The archive reader and the pack resolver have to agree exactly about
/// this, so they ask the same function rather than each carrying a copy of
/// the rule.
pub fn is_distributable_asset_path(path: &str) -> bool {
    asset_path::is_distributable_asset(path) && asset_path::has_canonical_asset_spelling(path)
}

/// Everything a pack manifest has to be true of, whichever way the pack
/// arrived.
///
/// A pack compiled into this binary and a pack downloaded from a registry
/// are held to one set of rules, checked here, because a pack that only
/// passes when it comes from a trusted place is not checked at all.
pub fn validate_manifest(name: &str, manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1 && manifest.name == name,
        "invalid pack identity/version"
    );
    if manifest.metadata.kind == PackKind::Graph {
        crate::graph_package::graph_manifest_from_pack(manifest)?;
    } else {
        anyhow::ensure!(manifest.graph.is_empty(), "unexpected manifest fields");
    }
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents || manifest.metadata.dependencies.is_empty(),
        "only document packs support package dependencies; nested graph/asset dependencies are unsupported"
    );
    anyhow::ensure!(
        !manifest.description.trim().is_empty() && !manifest.metadata.authors.is_empty(),
        "pack needs description and authors"
    );
    anyhow::ensure!(is_valid_pack_name(name), "pack name must be snake_case");
    // The namespace becomes half a registry coordinate and a path segment
    // in more than one store, so it is held to the same rule as the name
    // rather than passed through as free text.
    anyhow::ensure!(
        is_valid_pack_name(&manifest.metadata.namespace),
        "pack namespace must be snake_case: {:?}",
        manifest.metadata.namespace
    );
    let mut unique = BTreeSet::new();
    for path in &manifest.metadata.assets {
        anyhow::ensure!(
            asset_path::is_distributable_asset(path),
            "unsafe/private pack asset: {path}"
        );
        anyhow::ensure!(
            asset_path::has_canonical_asset_spelling(path),
            "non-canonical pack asset spelling: {path}"
        );
        anyhow::ensure!(unique.insert(path), "duplicate asset {path}");
    }
    anyhow::ensure!(
        unique.contains(&"README.md".to_owned()),
        "pack must declare README.md"
    );

    let mut plugin_names = BTreeSet::new();
    for plugin in &manifest.metadata.plugins {
        plugin.validate()?;
        anyhow::ensure!(
            plugin_names.insert(plugin.name.as_str()),
            "pack declares the plugin {:?} twice",
            plugin.name
        );
        // A plugin's artifact travels as a declared asset like anything
        // else, so the pack's own digest covers it and nothing can be
        // swapped underneath the name it was admitted under.
        anyhow::ensure!(
            unique.contains(&plugin.artifact),
            "pack declares the plugin {:?} but not its artifact {:?} as an asset",
            plugin.name,
            plugin.artifact
        );
    }
    anyhow::ensure!(
        manifest.metadata.kind != PackKind::Plugins || !manifest.metadata.plugins.is_empty(),
        "a plugins pack must declare at least one plugin"
    );
    Ok(())
}

/// The paths that make up a pack's identity: its manifest and every asset
/// it declares, sorted, each once.
pub fn declared_paths(manifest: &PackManifest) -> Vec<String> {
    let mut paths = manifest.metadata.assets.clone();
    paths.push("manifest.json".to_owned());
    paths.sort();
    paths.dedup();
    paths
}

/// A pack's digest, over its declared contents rather than over whatever
/// container carried them.
///
/// This is what makes a pack the same pack wherever it came from: the same
/// documents give the same digest whether they were compiled into a binary
/// or downloaded and unpacked, so a container that recompresses differently
/// does not change the pack's identity, and neither does the route it took.
pub fn digest_declared_assets<'a>(
    manifest: &PackManifest,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for path in declared_paths(manifest) {
        let bytes =
            asset(&path).with_context(|| format!("pack references missing asset {path:?}"))?;
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn resolve_pack(name: &str) -> Result<ResolvedPack> {
    anyhow::ensure!(
        BUNDLED_PACK_NAMES.contains(&name),
        "unknown pack {name:?}; use gents pack list"
    );
    let bytes = bundled_pack_asset(name, "manifest.json").context("missing manifest")?;
    let manifest: PackManifest = serde_json::from_slice(bytes)?;
    validate_manifest(name, &manifest)?;
    let digest = crate::graph_package::digest_assets(name, &declared_paths(&manifest))?;
    Ok(ResolvedPack { manifest, digest })
}

pub fn pack_catalog() -> Result<Vec<PackManifest>> {
    BUNDLED_PACK_NAMES
        .iter()
        .map(|name| Ok(resolve_pack(name)?.manifest))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_graph_manifest_rejects_unknown_fields() {
        let pack = crate::graph_package::load_bundled_graph_package("code_review").unwrap();
        let mut value = serde_json::to_value(&pack.manifest).unwrap();
        value["unexpected_field"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<crate::graph_package::GraphPackageManifest>(value).is_err()
        );
        let mut distribution = resolve_pack("code_review").unwrap().manifest;
        distribution
            .graph
            .insert("unexpected_field".to_owned(), serde_json::json!(true));
        assert!(crate::graph_package::graph_manifest_from_pack(&distribution).is_err());
    }
    #[test]
    fn all_packs_resolve_with_declared_assets_and_dependencies() {
        let catalog = pack_catalog().unwrap();
        assert!(catalog.len() >= 11);
        for pack in catalog {
            for dependency in pack.metadata.dependencies {
                resolve_pack(&dependency).unwrap();
            }
        }
        assert!(resolve_pack("code-review").is_err());
        assert!(resolve_pack("../code_review").is_err());
    }

    /// A minimal, otherwise-valid plugin, so each test below changes
    /// exactly the one field it means to check.
    fn valid_plugin() -> PackPlugin {
        PackPlugin {
            name: "format_check".to_owned(),
            description: "Checks formatting".to_owned(),
            artifact: "plugins/format_check.afb".to_owned(),
            source: None,
            language: "rust".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            manifold: None,
        }
    }

    #[test]
    fn a_plugin_artifact_must_be_a_compiled_afb_path() {
        let plugin = PackPlugin {
            artifact: "plugins/format_check.wasm".to_owned(),
            ..valid_plugin()
        };
        let error = plugin
            .validate()
            .expect_err("a bare .wasm artifact must be refused");
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains(".afb"), "{message}");
    }

    #[test]
    fn a_plugin_artifact_must_live_under_the_plugins_prefix() {
        let plugin = PackPlugin {
            artifact: "tools/format_check.afb".to_owned(),
            ..valid_plugin()
        };
        let error = plugin
            .validate()
            .expect_err("an artifact outside plugins/ must be refused");
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("plugins/"), "{message}");
        assert!(message.contains("tools/format_check.afb"), "{message}");
    }

    #[test]
    fn a_plugin_with_an_unknown_language_is_refused_naming_it_and_the_supported_set() {
        let plugin = PackPlugin {
            language: "haskell".to_owned(),
            ..valid_plugin()
        };
        let error = plugin
            .validate()
            .expect_err("an unsupported language must be refused");
        let message = format!("{error:#}");
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("haskell"), "{message}");
        assert!(message.contains("rust"), "{message}");
        assert!(message.contains("ruby"), "{message}");
    }

    /// Python is one of the languages a pack may declare. It compiles to
    /// an emscripten-pyodide bundle rather than a WASI command, and that
    /// bundle runs under every bound a plugin call applies (fuel, memory,
    /// wall clock, stdin), so refusing it here would refuse a language the
    /// runtime can in fact contain. Whether a *built* artifact is bounded
    /// is checked against the artifact itself, in `crate::plugin`.
    #[test]
    fn python_is_a_language_a_pack_may_declare() {
        for language in ["python", "PYTHON", "py", "Py"] {
            let plugin = PackPlugin {
                language: language.to_owned(),
                ..valid_plugin()
            };
            plugin
                .validate()
                .unwrap_or_else(|error| panic!("{language:?} must be accepted: {error:#}"));
        }
    }

    #[test]
    fn every_supported_language_identifier_is_accepted_case_insensitively() {
        for language in SUPPORTED_PLUGIN_LANGUAGES {
            let plugin = PackPlugin {
                language: language.to_ascii_uppercase(),
                ..valid_plugin()
            };
            plugin
                .validate()
                .unwrap_or_else(|error| panic!("{language:?} must be accepted: {error:#}"));
        }
    }
}
