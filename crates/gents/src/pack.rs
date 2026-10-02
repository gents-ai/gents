//! Distribution/catalog boundary. Execution and writes remain owned by the
//! graph installer and desired-state installer, not by package resolution.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod home_install;
mod inference;
mod installation;
pub mod interpolate;
mod loader;
mod provenance;
pub use home_install::{
    forget_home_install, list_home_installs, read_home_install, write_home_install, HomePackInstall,
};
pub use inference::{
    bind_pack_install_config, inference_profile_options, inspect_pack_inference_bindings,
    install_pack_documents, preview_pack_inference_bindings, PackInferenceBindingPreview,
    PackInferenceProfileOption,
};
pub use installation::{
    installed_packs, list_installed_packs, read_installed_pack, referenced_pack_digests,
    remove_pack, DriftPolicy, InstallReport, InstalledPack, InstalledPackPlugin, PackIdentity,
    RemoveReport, Retained,
};
pub(crate) use installation::{observe_graph_install_in_txn, record_graph_install_in_txn};
pub use loader::{decode_pack_config, load_pack_config, pin_pack_plugins};
pub(crate) use provenance::{pack_artifact_document_digest, prepare_pack_plan_in_txn};
pub use provenance::{pack_document_digests, pack_origin_from_tags, pack_origin_tag};

#[path = "pack_asset_path.rs"]
mod asset_path;

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
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    pub assets: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub dependencies: Vec<String>,
    /// Pack-local inference roles bound to existing principal profiles before
    /// any document is written. Slot names are authoring/install vocabulary;
    /// installed behaviors retain only their canonical profile reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inference_slots: Vec<PackInferenceSlot>,
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

/// One stable, pack-local inference role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInferenceSlot {
    pub name: String,
    pub description: String,
    /// Canonical behavior IDs whose authored profile reference names this slot.
    /// Empty for a slot only plugins use ([`PackPlugin::model_slot`]).
    #[serde(default)]
    pub behaviors: Vec<String>,
    /// A slot an install may leave unbound, and bind later. Only a slot that
    /// names no behavior can be optional: an unbound behavior has no profile.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

/// Complete explicit slot-to-existing-profile selection supplied at install.
pub type PackInferenceBindings = BTreeMap<String, String>;

/// Authored marker used only while decoding a pack. Installation replaces it
/// with an existing principal-owned profile ID before reference validation.
pub const INFERENCE_SLOT_REFERENCE_PREFIX: &str = "gents:inference-slot:";

pub fn inference_slot_reference(name: &str) -> String {
    format!("{INFERENCE_SLOT_REFERENCE_PREFIX}{name}")
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
    /// Markdown a model reads to use the plugin as a tool:
    /// `plugins/<name>/TOOL.md`. Absent, the description is all it gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Lets one directory (or a file inside one) be bound into a call, named
    /// fresh at every call site rather than granted once at install: an
    /// operator flag, or a path in a graph node's source document or a model's
    /// tool arguments that must resolve inside a folder the operator allowed
    /// (see `crate::plugin::allowed`). Absent means the plugin can never be
    /// bound. No `deny_unknown_fields` on [`PackPlugin`] itself, so an
    /// older gents ignores this field entirely on a manifest that declares
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_dir: Option<PluginDirBinding>,
    /// Resource ceiling this plugin declares it needs, raising
    /// [`crate::plugin::PluginBudget::for_plugin`]'s default rather than
    /// capping a caller's own budget. Each field must not exceed this
    /// module's host ceiling; see [`PackPlugin::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<PluginLimits>,
    /// The pack's inference slot this plugin may call a model through. While
    /// the slot is bound for the installation the host adds `"model_calls":
    /// true` to the plugin's input and answers its model requests (see
    /// `crate::plugin::model_calls`); unbound, the plugin runs as it always
    /// did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_slot: Option<String>,
}

/// How much of a bound directory a plugin uses. Ordered: `ReadWrite`
/// includes `Read`, so a folder allowed `ReadWrite` also serves a reader.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindAccess {
    #[default]
    Read,
    ReadWrite,
}

impl BindAccess {
    /// The manifest and operator spelling: `read` or `read_write`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read_write",
        }
    }

    fn is_read(&self) -> bool {
        *self == Self::Read
    }
}

impl std::str::FromStr for BindAccess {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> anyhow::Result<Self> {
        match text {
            "read" => Ok(Self::Read),
            "read_write" => Ok(Self::ReadWrite),
            other => anyhow::bail!("{other:?} is not an access; use read or read_write"),
        }
    }
}

/// Where a plugin's `bind_dir` binds: which input field carries the
/// canonical bound path, what a consenting operator is shown for it, and
/// how much of the directory the plugin uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDirBinding {
    /// A property of `input_schema` (or of each `oneOf` branch that
    /// declares properties): the argument
    /// [`crate::plugin::PluginRunner::call_bound`] overwrites with the
    /// canonical bound path, so a plugin can never point itself at a
    /// different directory than the one its caller named.
    pub input_field: String,
    /// A second string property of `input_schema` that the call fills with the
    /// canonical path the caller named, overwriting anything passed. For a
    /// single file `input_field` holds a short-lived link to it, so a plugin
    /// that hands the path on (a graph node writing the next stage's input)
    /// reads the real one here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_field: Option<String>,
    /// Shown to an operator deciding whether to bind this plugin.
    pub description: String,
    /// `read` (the default) or `read_write`. A caller may bind it only
    /// where the operator allowed at least this much.
    #[serde(default, skip_serializing_if = "BindAccess::is_read")]
    pub access: BindAccess,
}

/// A plugin's declared resource ceiling: what
/// [`crate::plugin::PluginBudget::for_plugin`] raises the default to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PluginLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_mib: Option<u32>,
}

/// Largest `TOOL.md` a plugin may ship: a model reads it on every turn the
/// tool is offered.
pub const MAX_TOOL_INSTRUCTIONS_BYTES: usize = 64 * 1024;

/// A plugin's `TOOL.md` as text, refused when it is not UTF-8 or too long to
/// hand a model.
pub fn tool_instructions(plugin: &str, bytes: &[u8]) -> Result<String> {
    anyhow::ensure!(
        bytes.len() <= MAX_TOOL_INSTRUCTIONS_BYTES,
        "plugin {plugin:?}'s TOOL.md is {} bytes; the limit is {} KiB",
        bytes.len(),
        MAX_TOOL_INSTRUCTIONS_BYTES / 1024
    );
    String::from_utf8(bytes.to_vec())
        .with_context(|| format!("plugin {plugin:?}'s TOOL.md is not UTF-8 text"))
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
/// Kept in sync by hand with afterburner's own list; the ceiling is a
/// shared, lightweight language-id crate both sides could depend on if this
/// ever drifts.
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
        if let Some(instructions) = &self.instructions {
            anyhow::ensure!(
                *instructions == format!("{PLUGIN_ARTIFACT_PREFIX}{}/TOOL.md", self.name),
                "plugin {:?} instructions must be plugins/{}/TOOL.md, got {instructions:?}",
                self.name,
                self.name
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
        if let Some(bind_dir) = &self.bind_dir {
            anyhow::ensure!(
                !bind_dir.input_field.trim().is_empty(),
                "plugin {:?} bind_dir.input_field must not be blank",
                self.name
            );
            anyhow::ensure!(
                !bind_dir.description.trim().is_empty(),
                "plugin {:?} bind_dir needs a description; it is what an operator is shown",
                self.name
            );
            anyhow::ensure!(
                schema_declares_string_property(&self.input_schema, &bind_dir.input_field),
                "plugin {:?} bind_dir.input_field {:?} must be a string property of \
                 input_schema, or of at least one oneOf branch, since call_bound always \
                 injects a string",
                self.name,
                bind_dir.input_field
            );
            if let Some(original) = &bind_dir.original_field {
                anyhow::ensure!(
                    original != &bind_dir.input_field
                        && schema_declares_string_property(&self.input_schema, original),
                    "plugin {:?} bind_dir.original_field {:?} must be a string property of \
                     input_schema other than input_field",
                    self.name,
                    original
                );
            }
            // The directory a caller binds is authority granted fresh at
            // every call (see `crate::plugin::BoundDir`), never a standing
            // one; a plugin that also declared its own `fs` grant would
            // read both, which is not a ceiling this field can express, so
            // the two are mutually exclusive.
            let declares_fs = !matches!(
                crate::plugin::authority::declared_manifold(self)?.fs,
                afterburner_core::manifold::FsAccess::None
            );
            anyhow::ensure!(
                !declares_fs,
                "plugin {:?} declares both bind_dir and a manifold fs grant; a directory bound \
                 per call and a standing filesystem grant cannot be expressed together",
                self.name
            );
        }
        if let Some(limits) = &self.limits {
            if let Some(memory_mib) = limits.memory_mib {
                anyhow::ensure!(
                    (1..=crate::plugin::MAX_DECLARED_MEMORY_MIB).contains(&memory_mib),
                    "plugin {:?} declares memory_mib {memory_mib}, but it {}",
                    self.name,
                    limit_range_reason(memory_mib, crate::plugin::MAX_DECLARED_MEMORY_MIB, "MiB")
                );
            }
            if let Some(wall_clock_secs) = limits.wall_clock_secs {
                anyhow::ensure!(
                    (1..=crate::plugin::MAX_DECLARED_WALL_CLOCK_SECS).contains(&wall_clock_secs),
                    "plugin {:?} declares wall_clock_secs {wall_clock_secs}, but it {}",
                    self.name,
                    limit_range_reason(
                        wall_clock_secs,
                        crate::plugin::MAX_DECLARED_WALL_CLOCK_SECS,
                        "s"
                    )
                );
            }
            if let Some(max_output_mib) = limits.max_output_mib {
                anyhow::ensure!(
                    (1..=crate::plugin::MAX_DECLARED_OUTPUT_MIB).contains(&max_output_mib),
                    "plugin {:?} declares max_output_mib {max_output_mib}, but it {}",
                    self.name,
                    limit_range_reason(
                        max_output_mib,
                        crate::plugin::MAX_DECLARED_OUTPUT_MIB,
                        "MiB"
                    )
                );
            }
        }
        Ok(())
    }
}

/// Why a declared limit outside `1..=ceiling` was refused: the operator is
/// told the actual reason (zero is never a valid budget) rather than always
/// being told it is over the ceiling, which is only true above it.
fn limit_range_reason(value: u32, ceiling: u32, unit: &str) -> String {
    if value == 0 {
        "must be at least 1".to_owned()
    } else {
        format!("must be at most the host ceiling of {ceiling} {unit}")
    }
}

/// Whether `field` is a string property `input_schema` declares directly,
/// or that at least one of its `oneOf` branches declares: `call_bound`
/// always injects a string, so a field declared with another type could
/// never be satisfied, and a field absent from every location declares
/// nothing to bind.
fn schema_declares_string_property(input_schema: &serde_json::Value, field: &str) -> bool {
    let is_string_property = |schema: &serde_json::Value| {
        schema
            .get("properties")
            .and_then(|properties| properties.get(field))
            .and_then(|property| property.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("string")
    };
    if is_string_property(input_schema) {
        return true;
    }
    input_schema
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|branches| branches.iter().any(is_string_property))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(flatten)]
    pub metadata: PackMetadata,
    /// Declared asset decoded as PackConfig by the common loader. Required for
    /// document/graph packs; absent for asset-only packs. Sidecars are relative
    /// to this config asset. Graph topology/capabilities live in this same bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub schemas: Vec<String>,
    /// Required for graph compilation; absent for packs without graph topology.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub external_dependencies: Vec<PackageExternalDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageExternalDependency {
    pub service_id: String,
    pub description: String,
    pub repository_url: String,
    pub install_command: String,
}

/// Installation scope shared by document and graph packs. Logical references
/// resolve through the same canonical configuration loader. Graph installation
/// adds topology/revision validation, not behavior/model selection overrides.
/// Before strict decoding, fill omitted root owners from this explicit scope;
/// reject mismatched explicit owners. Never rewrite target/caller/signer DIDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInstallOptions {
    pub agent_did: String,
}

/// Return whether `name` is admissible at the pack catalog and source-pack
/// boundaries. Keep callers on this owner instead of growing parallel name
/// validators in adapters.
pub fn is_valid_pack_name(name: &str) -> bool {
    asset_path::is_snake_case_name(name)
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
    validate_pack_manifest(manifest)
}

/// Distribution validation shared by every pack loader.
pub fn validate_pack_manifest(manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1,
        "unsupported pack manifest version"
    );
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents || manifest.metadata.dependencies.is_empty(),
        "only document packs support package dependencies; nested graph/asset dependencies are unsupported"
    );
    for dependency in &manifest.metadata.dependencies {
        anyhow::ensure!(
            !dependency.contains('@'),
            "dependency {dependency:?} must be a coordinate (name or ns/name), not a pinned version"
        );
        let (namespace, name) = dependency
            .split_once('/')
            .unwrap_or((crate::pack_archive::DEFAULT_NAMESPACE, dependency.as_str()));
        anyhow::ensure!(
            is_valid_pack_name(namespace) && is_valid_pack_name(name),
            "dependency {dependency:?} is not a valid pack coordinate"
        );
    }
    anyhow::ensure!(
        !manifest.description.trim().is_empty() && !manifest.metadata.authors.is_empty(),
        "pack needs description and authors"
    );
    anyhow::ensure!(
        is_valid_pack_name(&manifest.name),
        "pack name must be snake_case"
    );
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
        if let Some(instructions) = &plugin.instructions {
            anyhow::ensure!(
                unique.contains(instructions),
                "pack declares the plugin {:?} but not its instructions {instructions:?} as an asset",
                plugin.name
            );
        }
    }
    anyhow::ensure!(
        manifest.metadata.kind != PackKind::Plugins || !manifest.metadata.plugins.is_empty(),
        "a plugins pack must declare at least one plugin"
    );

    let mut slot_names = BTreeSet::new();
    let mut slot_behaviors = BTreeSet::new();
    for slot in &manifest.metadata.inference_slots {
        anyhow::ensure!(
            is_valid_pack_name(&slot.name),
            "inference slot name must be snake_case: {:?}",
            slot.name
        );
        anyhow::ensure!(
            slot_names.insert(slot.name.as_str()),
            "pack declares inference slot {:?} twice",
            slot.name
        );
        anyhow::ensure!(
            !slot.description.trim().is_empty(),
            "inference slot {:?} needs a description",
            slot.name
        );
        anyhow::ensure!(
            slot.optional == slot.behaviors.is_empty(),
            "inference slot {:?} must name at least one behavior, unless it is optional, and \
             an optional slot names none",
            slot.name
        );
        let mut local = BTreeSet::new();
        for behavior in &slot.behaviors {
            anyhow::ensure!(
                !behavior.trim().is_empty() && local.insert(behavior.as_str()),
                "inference slot {:?} repeats or has a blank behavior",
                slot.name
            );
            anyhow::ensure!(
                slot_behaviors.insert(behavior.as_str()),
                "behavior {behavior:?} belongs to more than one inference slot"
            );
        }
    }
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents
            || manifest.metadata.kind == PackKind::Graph
            || manifest
                .metadata
                .inference_slots
                .iter()
                .all(|slot| slot.optional),
        "asset and plugins packs can declare only optional inference slots"
    );
    let mut plugin_slots = BTreeSet::new();
    for plugin in &manifest.metadata.plugins {
        if let Some(slot) = &plugin.model_slot {
            anyhow::ensure!(
                slot_names.contains(slot.as_str()),
                "plugin {:?} names the model slot {slot:?}, which the pack does not declare",
                plugin.name
            );
            anyhow::ensure!(
                manifest.metadata.inference_slots.iter().any(|declared| {
                    declared.name == *slot && declared.optional && declared.behaviors.is_empty()
                }),
                "plugin {:?} model slot {slot:?} must be optional and have no behaviors",
                plugin.name
            );
            plugin_slots.insert(slot.as_str());
        }
    }
    for slot in manifest
        .metadata
        .inference_slots
        .iter()
        .filter(|slot| slot.optional)
    {
        anyhow::ensure!(
            plugin_slots.contains(slot.name.as_str()),
            "optional inference slot {:?} is used by no plugin",
            slot.name
        );
    }

    match manifest.metadata.kind {
        PackKind::Documents | PackKind::Graph => {
            let config = manifest
                .config
                .as_deref()
                .context("document/graph pack requires a config asset")?;
            anyhow::ensure!(
                manifest.metadata.assets.iter().any(|asset| asset == config),
                "config asset must be declared"
            );
        }
        PackKind::Assets | PackKind::Plugins => {
            anyhow::ensure!(
                manifest.config.is_none(),
                "asset-only pack cannot declare configuration"
            );
            anyhow::ensure!(
                manifest.schemas.is_empty(),
                "asset-only pack cannot declare installed schemas"
            );
        }
    }
    let mut schemas = BTreeSet::new();
    for schema in &manifest.schemas {
        anyhow::ensure!(
            manifest.metadata.assets.contains(schema),
            "schema asset must be declared: {schema}"
        );
        anyhow::ensure!(schemas.insert(schema), "duplicate schema asset: {schema}");
    }
    if manifest.metadata.kind == PackKind::Graph {
        anyhow::ensure!(
            manifest.compiler_version.as_deref() == Some(crate::graph_pipeline::COMPILER_VERSION),
            "graph pack compiler version does not match runtime"
        );
    } else {
        anyhow::ensure!(
            manifest.compiler_version.is_none(),
            "non-graph pack cannot select a graph compiler"
        );
    }
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
    let mut digest = PackDigester::default();
    for path in declared_paths(manifest) {
        let bytes =
            asset(&path).with_context(|| format!("pack references missing asset {path:?}"))?;
        digest.begin_entry(&path, bytes.len() as u64);
        digest.update(bytes);
    }
    Ok(digest.finish())
}

/// The pack digest, computed incrementally: each declared path in
/// [`declared_paths`] order, as its length, its bytes, its content length and
/// its content. The one definition every reader and writer of a pack uses,
/// so a pack streamed through a `.pack` file and one compiled into the binary
/// cannot hash differently.
#[derive(Default)]
pub struct PackDigester(sha2::Sha256);

impl PackDigester {
    /// Starts an entry whose content is `len` bytes, fed through [`Self::update`].
    pub fn begin_entry(&mut self, path: &str, len: u64) {
        use sha2::Digest;
        self.0.update((path.len() as u64).to_be_bytes());
        self.0.update(path.as_bytes());
        self.0.update(len.to_be_bytes());
    }

    pub fn update(&mut self, bytes: &[u8]) {
        sha2::Digest::update(&mut self.0, bytes);
    }

    /// The digest as `sha256:{hex}`.
    pub fn finish(self) -> String {
        format!("sha256:{:x}", sha2::Digest::finalize(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            instructions: None,
            bind_dir: None,
            limits: None,
            model_slot: None,
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

    fn plugins_pack(slots: serde_json::Value, model_slot: Option<&str>) -> PackManifest {
        let mut plugin = serde_json::to_value(valid_plugin()).unwrap();
        plugin["model_slot"] = serde_json::json!(model_slot);
        serde_json::from_value(serde_json::json!({
            "manifest_version": 1,
            "name": "ocr",
            "version": "1.0.0",
            "description": "reads pages",
            "authors": ["tests"],
            "kind": "plugins",
            "assets": ["README.md", "plugins/format_check.afb"],
            "inference_slots": slots,
            "plugins": [plugin],
        }))
        .unwrap()
    }

    #[test]
    fn a_plugin_may_name_an_optional_slot_the_pack_declares() {
        let optional =
            serde_json::json!([{"name": "remote_ocr", "description": "d", "optional": true}]);
        validate_pack_manifest(&plugins_pack(optional.clone(), Some("remote_ocr"))).unwrap();
        validate_pack_manifest(&plugins_pack(serde_json::json!([]), None)).unwrap();
        let unknown = validate_pack_manifest(&plugins_pack(optional.clone(), Some("other")));
        assert!(format!("{:#}", unknown.unwrap_err()).contains("does not declare"));
        let unused = validate_pack_manifest(&plugins_pack(optional, None));
        assert!(format!("{:#}", unused.unwrap_err()).contains("used by no plugin"));
        let undeclared =
            validate_pack_manifest(&plugins_pack(serde_json::json!([]), Some("remote_ocr")));
        assert!(undeclared.is_err());
    }

    #[test]
    fn generated_plugin_model_slots_require_optional_behavior_free_declarations() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases;
        for case in cases["model_slots"].as_array().unwrap() {
            let slots = if case["declared"].as_bool().unwrap() {
                serde_json::json!([{
                    "name": "remote_ocr", "description": "d",
                    "optional": case["optional"],
                    "behaviors": if case["behavior_free"].as_bool().unwrap() { vec![] } else { vec!["scan"] },
                }])
            } else {
                serde_json::json!([])
            };
            let mut manifest = plugins_pack(slots, Some("remote_ocr"));
            manifest.metadata.kind = PackKind::Documents;
            manifest.config = Some("config.json".to_owned());
            manifest.metadata.assets.push("config.json".to_owned());
            assert_eq!(
                validate_pack_manifest(&manifest).is_ok(),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn only_a_slot_without_behaviors_can_be_optional() {
        let required = serde_json::json!([{"name": "remote_ocr", "description": "d"}]);
        assert!(validate_pack_manifest(&plugins_pack(required, Some("remote_ocr"))).is_err());
        let with_behavior = serde_json::json!([{
            "name": "remote_ocr", "description": "d", "optional": true, "behaviors": ["scan"]
        }]);
        assert!(validate_pack_manifest(&plugins_pack(with_behavior, Some("remote_ocr"))).is_err());
    }

    fn bindable_plugin() -> PackPlugin {
        PackPlugin {
            input_schema: serde_json::json!({"type": "object", "properties": {"root": {"type": "string"}}}),
            bind_dir: Some(PluginDirBinding {
                input_field: "root".to_owned(),
                original_field: None,
                description: "the directory to scan".to_owned(),
                access: BindAccess::Read,
            }),
            ..valid_plugin()
        }
    }

    #[test]
    fn bind_dir_access_defaults_to_read_and_names_only_read_or_read_write() {
        let parse = |access: &str| {
            serde_json::from_str::<PluginDirBinding>(&format!(
                r#"{{"input_field":"root","description":"d"{access}}}"#
            ))
        };
        assert_eq!(parse("").unwrap().access, BindAccess::Read);
        assert_eq!(
            parse(r#","access":"read_write""#).unwrap().access,
            BindAccess::ReadWrite
        );
        assert!(parse(r#","access":"write""#).is_err());
        let plain = serde_json::to_value(parse("").unwrap()).unwrap();
        assert!(
            plain.get("access").is_none(),
            "the default is not written out"
        );
    }

    #[test]
    fn bind_dir_requires_its_input_field_to_be_a_schema_property() {
        bindable_plugin()
            .validate()
            .expect("root is a declared property");

        let mut missing = bindable_plugin();
        missing.input_schema = serde_json::json!({"type": "object"});
        let error = missing.validate().expect_err("root is not declared");
        assert!(format!("{error:#}").contains("root"));
    }

    #[test]
    fn bind_dir_original_field_is_a_second_string_property() {
        let mut plugin = bindable_plugin();
        plugin.input_schema = serde_json::json!({"type": "object", "properties": {
            "root": {"type": "string"}, "root_original": {"type": "string"}}});
        plugin.bind_dir.as_mut().unwrap().original_field = Some("root_original".into());
        plugin
            .validate()
            .expect("a declared second string property");

        plugin.bind_dir.as_mut().unwrap().original_field = Some("root".into());
        assert!(plugin.validate().is_err(), "it cannot be the input field");
        plugin.bind_dir.as_mut().unwrap().original_field = Some("elsewhere".into());
        let error = plugin.validate().expect_err("it must be declared");
        assert!(format!("{error:#}").contains("original_field"));
    }

    #[test]
    fn bind_dir_accepts_a_property_declared_by_one_one_of_branch() {
        // The secscan shape: `root` is required in one branch and absent
        // from the other (which takes `files` instead), so requiring every
        // branch to declare it would refuse a pack that never asked for
        // that.
        let mut plugin = bindable_plugin();
        plugin.input_schema = serde_json::json!({
            "oneOf": [
                {"properties": {"root": {"type": "string"}}},
                {"properties": {"files": {"type": "array"}}},
            ]
        });
        plugin
            .validate()
            .expect("one branch declaring root as a string is enough");

        plugin.input_schema = serde_json::json!({
            "oneOf": [
                {"properties": {"other": {"type": "string"}}},
                {"properties": {"files": {"type": "array"}}},
            ]
        });
        assert!(plugin.validate().is_err(), "no branch declares root at all");
    }

    #[test]
    fn bind_dir_refuses_a_non_string_input_field() {
        let mut plugin = bindable_plugin();
        plugin.input_schema = serde_json::json!({
            "type": "object", "properties": {"root": {"type": "integer"}}
        });
        let error = plugin
            .validate()
            .expect_err("call_bound always injects a string; a non-string field can never match");
        assert!(format!("{error:#}").contains("string"));

        plugin.input_schema = serde_json::json!({
            "oneOf": [{"properties": {"root": {"type": "integer"}}}]
        });
        assert!(
            plugin.validate().is_err(),
            "a non-string oneOf branch property must also be refused"
        );
    }

    #[test]
    fn bind_dir_and_a_standing_fs_grant_are_mutually_exclusive() {
        let mut plugin = bindable_plugin();
        plugin.manifold = Some(serde_json::json!({
            "fs": {"ReadOnly": ["/data"]}, "net": "None", "env": "None",
            "crypto": false, "child_process": false
        }));
        let error = plugin
            .validate()
            .expect_err("bind_dir plus a standing fs grant must be refused");
        assert!(format!("{error:#}").contains("bind_dir"));

        // A manifold that asks for something else, but not fs, is fine.
        let mut plugin = bindable_plugin();
        plugin.manifold = Some(serde_json::json!({
            "fs": "None", "net": "None", "env": {"AllowList": ["HOME"]},
            "crypto": false, "child_process": false
        }));
        plugin
            .validate()
            .expect("a non-fs grant alongside bind_dir is fine");
    }

    #[test]
    fn limits_must_not_exceed_the_host_ceiling() {
        let mut plugin = valid_plugin();
        plugin.limits = Some(PluginLimits {
            memory_mib: Some(crate::plugin::MAX_DECLARED_MEMORY_MIB),
            wall_clock_secs: Some(crate::plugin::MAX_DECLARED_WALL_CLOCK_SECS),
            max_output_mib: Some(crate::plugin::MAX_DECLARED_OUTPUT_MIB),
        });
        plugin.validate().expect("exactly the ceiling is fine");

        plugin.limits = Some(PluginLimits {
            memory_mib: Some(crate::plugin::MAX_DECLARED_MEMORY_MIB + 1),
            ..Default::default()
        });
        let error = plugin
            .validate()
            .expect_err("over the ceiling must be refused");
        assert!(format!("{error:#}").contains("memory_mib"));

        plugin.limits = Some(PluginLimits {
            wall_clock_secs: Some(crate::plugin::MAX_DECLARED_WALL_CLOCK_SECS + 1),
            ..Default::default()
        });
        assert!(plugin.validate().is_err());

        plugin.limits = Some(PluginLimits {
            max_output_mib: Some(crate::plugin::MAX_DECLARED_OUTPUT_MIB + 1),
            ..Default::default()
        });
        assert!(plugin.validate().is_err());
    }

    #[test]
    fn a_zero_limit_is_refused_for_being_zero_not_for_being_over_the_ceiling() {
        let mut plugin = valid_plugin();
        plugin.limits = Some(PluginLimits {
            memory_mib: Some(0),
            ..Default::default()
        });
        let error = plugin.validate().expect_err("zero is never a valid budget");
        let message = format!("{error:#}");
        assert!(message.contains("at least 1"), "{message}");
        assert!(!message.contains("ceiling"), "{message}");
    }

    fn documents_manifest(dependencies: Vec<&str>) -> PackManifest {
        serde_json::from_value(serde_json::json!({
            "manifest_version": 1,
            "name": "dep_test",
            "version": "1.0.0",
            "description": "a test documents pack",
            "authors": ["gents"],
            "kind": "documents",
            "assets": ["README.md", "pack_config.json"],
            "config": "pack_config.json",
            "dependencies": dependencies,
        }))
        .unwrap()
    }

    #[test]
    fn dependency_coordinates_are_bare_or_namespaced_names_never_pinned() {
        validate_pack_manifest(&documents_manifest(vec!["acme/widget"])).unwrap();
        validate_pack_manifest(&documents_manifest(vec!["widget"])).unwrap();

        let pinned = validate_pack_manifest(&documents_manifest(vec!["acme/widget@1.0.0"]))
            .expect_err("a dependency must not pin a version");
        assert!(
            format!("{pinned:#}").contains("must be a coordinate"),
            "{pinned:#}"
        );

        let bad = validate_pack_manifest(&documents_manifest(vec!["Acme/Widget"]))
            .expect_err("a dependency coordinate must be snake_case");
        assert!(
            format!("{bad:#}").contains("is not a valid pack coordinate"),
            "{bad:#}"
        );
    }
}
