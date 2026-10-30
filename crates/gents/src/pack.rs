use anyhow::{Context, Result};
use gents_protocol::job::{PluginDataDeclaration, PluginJobDeclaration};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod authoring;
pub mod home_install;
mod inference;
mod installation;
pub mod interpolate;
mod loader;
pub mod local;
pub mod materialize;
pub mod plugin_lifecycle;
mod provenance;
#[cfg(test)]
mod publication_contract_tests;
pub use home_install::{
    forget_home_install, list_home_installs, list_home_installs_with_skips,
    list_readable_home_installs, read_home_install, record_after_install, write_home_install,
    HomePackInstall, InstalledVia, SkippedHomeInstall,
};
pub use inference::{
    bind_pack_install_config, inference_profile_options, inspect_pack_inference_bindings,
    install_pack_documents, install_pack_documents_with_expectations,
    preview_pack_inference_bindings, PackInferenceBindingPreview, PackInferenceProfileOption,
};
pub(crate) use installation::sdl_paths;
pub use installation::{
    apply_prepared_dependency_schemas, document_pack_schema_paths, ensure_plugins_unreferenced,
    install_documents_with_plugins, install_prepared_document_graph,
    install_prepared_document_pack, installed_pack_documents, installed_packs,
    list_installed_packs, plugin_data_unique_keys, prepare_document_pack_install,
    read_installed_pack, record_plugin_store_change, record_plugin_store_change_as,
    referenced_pack_digests, remove_pack, DriftPolicy, InstallReport, InstalledPack,
    InstalledPackPlugin, PackIdentity, PluginStoreChangeError, PreparedDependency,
    PreparedDocumentPackInstall, RemoveReport, Retained,
};
pub use loader::{
    decode_pack_config, ensure_pack_leaves_default_unselected, load_pack_config, pin_pack_plugins,
};
pub(crate) use provenance::pack_artifact_document_digest;
pub use provenance::{pack_document_digests, pack_origin_from_tags, pack_origin_tag};

#[path = "pack_asset_path.rs"]
mod asset_path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    Documents,
    Assets,
    Plugins,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMetadata {
    pub kind: PackKind,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inference_slots: Vec<PackInferenceSlot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PackPlugin>,
    /// The UX plugins this pack ships: modules the desktop webview loads,
    /// each a nav row, a page, a header action or a transcript directive.
    /// A UX plugin is an asset, never a document: it writes nothing to the
    /// agent's store and runs with the webview's authority, gated by what
    /// it declares here (see [`PackUxPlugin`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ux: Vec<PackUxPlugin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInferenceSlot {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
}

pub type PackInferenceBindings = BTreeMap<String, String>;

pub const INFERENCE_SLOT_REFERENCE_PREFIX: &str = "gents:inference-slot:";

pub fn inference_slot_reference(name: &str) -> String {
    format!("{INFERENCE_SLOT_REFERENCE_PREFIX}{name}")
}

fn default_namespace() -> String {
    crate::pack_archive::DEFAULT_NAMESPACE.to_owned()
}

fn undeclared_job() -> PluginJobDeclaration {
    PluginJobDeclaration(false)
}

fn job_undeclared(job: &PluginJobDeclaration) -> bool {
    !job.0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackPlugin {
    pub name: String,
    pub description: String,
    pub artifact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub language: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifold: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_dir: Option<PluginDirBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<PluginLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_slot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<PluginService>,
    /// Whether the plugin runs job steps; absent means it serves non-job
    /// verbs only.
    #[serde(default = "undeclared_job", skip_serializing_if = "job_undeclared")]
    pub job: PluginJobDeclaration,
    /// The collections, operations and host-stamped fields the plugin's data
    /// calls and writes may use; absent means it has no data access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<PluginDataDeclaration>,
    /// The host functions the artifact imports under the `gents` module;
    /// absent means it imports none and runs on WASI alone. Only a job plugin
    /// whose input names `script` may ask for `run_script`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_imports: Vec<HostImport>,
}

/// A host function a plugin artifact may import (wasm module `gents`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostImport {
    /// `run_script(ptr, len) -> i32` and `read_result(ptr, cap) -> i32`: the
    /// host evaluates the call's own `script` on the JSON the guest hands it.
    RunScript,
}

/// For literals that set only the fields they need.
impl Default for PackPlugin {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            artifact: String::new(),
            source: None,
            language: String::new(),
            input_schema: serde_json::Value::Null,
            manifold: None,
            instructions: None,
            bind_dir: None,
            limits: None,
            model_slot: None,
            service: None,
            job: undeclared_job(),
            data: None,
            host_imports: Vec::new(),
        }
    }
}

/// A pack-owned native process. Its first stdout line is a JSON object injected
/// as `service`; subsequent output is drained. `{pack}` and `{state}` are host
/// paths, never shell substitutions. Requires the existing child-process grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginService {
    pub argv: Vec<String>,
    pub startup_timeout_secs: u32,
    /// Let the WASM plugin handle unavailable native execution through its injected service input.
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub skip_when: std::collections::BTreeMap<String, serde_json::Value>,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindAccess {
    #[default]
    Read,
    ReadWrite,
}

impl BindAccess {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDirBinding {
    pub input_field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_field: Option<String>,
    pub description: String,
    #[serde(default, skip_serializing_if = "BindAccess::is_read")]
    pub access: BindAccess,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_fields: Vec<String>,
}

impl PluginDirBinding {
    pub fn call_access(&self, arguments: &serde_json::Value) -> BindAccess {
        match self.access {
            BindAccess::Read => BindAccess::Read,
            BindAccess::ReadWrite if self.write_fields.is_empty() => BindAccess::ReadWrite,
            BindAccess::ReadWrite if self.write_fields_set(arguments).is_empty() => {
                BindAccess::Read
            }
            BindAccess::ReadWrite => BindAccess::ReadWrite,
        }
    }

    pub fn write_fields_set(&self, arguments: &serde_json::Value) -> Vec<&str> {
        self.write_fields
            .iter()
            .filter(|field| {
                arguments
                    .get(field.as_str())
                    .is_some_and(|value| !value.is_null())
            })
            .map(String::as_str)
            .collect()
    }
}

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

/// Bounds model-facing instructions loaded from a pack so one document cannot
/// add unbounded prompt text or allocation to each provider turn.
pub const MAX_TOOL_INSTRUCTIONS_BYTES: usize = 64 * 1024;

/// Where a UX plugin's files must live inside a pack, by convention
/// (`ux/<name>/plugin.js`). Checked in [`PackUxPlugin::validate`].
pub const UX_PLUGIN_PREFIX: &str = "ux/";

/// The desktop surfaces a UX plugin may contribute to. Mirrors
/// `KNOWN_AREAS` in the desktop's `contrib/types.ts`; a manifest naming an
/// area outside this list is refused at the pack rather than at load.
pub const UX_PLUGIN_AREAS: &[&str] = &[
    "nav",
    "agent.sections",
    "session.header.actions",
    "transcript.directives",
    "plugin.pages",
];

/// One UX plugin a pack ships: a module the desktop webview evaluates.
///
/// Exactly one of `entry` (a file in the pack, under `ux/`) or `plugin`
/// (the name of one of this pack's `.afb` plugins, whose stdout for the
/// input `{"role":"ux"}` is `{"module":"<esm>","css":"<optional>"}`)
/// produces the module. Whichever produced it, the module passes the same
/// gate: an import allowlist and the declared-contributions check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackUxPlugin {
    /// Unique within the pack; the desktop shows it as `<ns>/<pack>/<name>`.
    pub name: String,
    /// What it adds, in the words the UX Plugins panel shows.
    pub description: String,
    /// A plain ESM file (no build) or a prebuilt bundle, under `ux/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// An optional stylesheet installed beside the module, under `ux/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    /// One of this pack's `plugins[]` whose run produces the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// What the plugin may register; the desktop refuses anything else.
    pub contributes: UxContributions,
    /// Whether it registers on install when the user has not chosen.
    /// Absent means yes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_enabled: Option<bool>,
    /// The datastore tools its pages may run, by surface: the host refuses
    /// any (surface, tool) pair not named here (`crate::ux_data`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<UxDataSurface>,
}

/// One `DatastoreToolSurface` a UX plugin's pages may call into, by the
/// surface's id on the agent's node, narrowed to the named tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UxDataSurface {
    pub surface: String,
    pub tools: Vec<String>,
}

/// Longest surface id a manifest may name.
pub const MAX_UX_DATA_SURFACE_ID_LEN: usize = 128;

/// A tool name a manifest may name: `[a-z][a-z0-9_]{0,63}`, the shape a
/// `DatastoreToolSurface` entry's `tool_name` takes.
pub fn is_valid_ux_data_tool_name(name: &str) -> bool {
    name.len() <= 64 && is_valid_pack_name(name)
}

/// What a UX plugin declares it contributes: the gate the desktop's
/// plugin context enforces on every `register` call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct UxContributions {
    /// Area ids, each one of [`UX_PLUGIN_AREAS`].
    #[serde(default)]
    pub areas: Vec<String>,
    /// Transcript directive names the plugin may claim (`::name{...}`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
}

/// A directive name a manifest may declare: `[a-z][a-z0-9-]{0,63}`,
/// the same shape the desktop parser accepts.
pub fn is_valid_directive_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl PackUxPlugin {
    /// The rules a UX plugin has to satisfy at the pack. `plugin_names` is
    /// the pack's declared `.afb` plugins, for the `plugin` producer.
    pub fn validate(&self, plugin_names: &BTreeSet<&str>) -> Result<()> {
        anyhow::ensure!(
            is_valid_pack_name(&self.name),
            "ux plugin name must be snake_case: {:?}",
            self.name
        );
        anyhow::ensure!(
            !self.description.trim().is_empty(),
            "ux plugin {:?} needs a description; it is what the UX Plugins panel shows",
            self.name
        );
        match (&self.entry, &self.plugin) {
            (Some(entry), None) => {
                anyhow::ensure!(
                    is_distributable_asset_path(entry),
                    "unsafe ux plugin entry path: {entry:?}"
                );
                anyhow::ensure!(
                    entry.starts_with(UX_PLUGIN_PREFIX) && entry.ends_with(".js"),
                    "ux plugin {:?} entry must be a .js file under {UX_PLUGIN_PREFIX}, got {entry:?}",
                    self.name
                );
            }
            (None, Some(plugin)) => {
                anyhow::ensure!(
                    plugin_names.contains(plugin.as_str()),
                    "ux plugin {:?} names the plugin {plugin:?}, which this pack does not declare",
                    self.name
                );
                anyhow::ensure!(
                    self.css.is_none(),
                    "ux plugin {:?} is produced by a plugin, which emits its own css; drop the css field",
                    self.name
                );
            }
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "ux plugin {:?} declares both entry and plugin; exactly one produces the module",
                    self.name
                )
            }
            (None, None) => anyhow::bail!(
                "ux plugin {:?} declares neither entry nor plugin; one must produce the module",
                self.name
            ),
        }
        if let Some(css) = &self.css {
            anyhow::ensure!(
                is_distributable_asset_path(css)
                    && css.starts_with(UX_PLUGIN_PREFIX)
                    && css.ends_with(".css"),
                "ux plugin {:?} css must be a .css file under {UX_PLUGIN_PREFIX}, got {css:?}",
                self.name
            );
        }
        anyhow::ensure!(
            !self.contributes.areas.is_empty(),
            "ux plugin {:?} must declare at least one area it contributes to",
            self.name
        );
        for area in &self.contributes.areas {
            anyhow::ensure!(
                UX_PLUGIN_AREAS.contains(&area.as_str()),
                "ux plugin {:?} declares the area {area:?}, which the desktop does not have; known \
                 areas: {}",
                self.name,
                UX_PLUGIN_AREAS.join(", ")
            );
        }
        for directive in &self.contributes.directives {
            anyhow::ensure!(
                is_valid_directive_name(directive),
                "ux plugin {:?} declares the directive {directive:?}; a directive name is \
                 lowercase letters, digits and dashes, starting with a letter",
                self.name
            );
        }
        anyhow::ensure!(
            self.contributes.directives.is_empty()
                || self
                    .contributes
                    .areas
                    .iter()
                    .any(|a| a == "transcript.directives"),
            "ux plugin {:?} declares directives but not the transcript.directives area",
            self.name
        );
        let mut surfaces = BTreeSet::new();
        for declared in &self.data {
            let surface = declared.surface.as_str();
            anyhow::ensure!(
                !surface.is_empty()
                    && !surface.chars().any(char::is_whitespace)
                    && surface.len() <= MAX_UX_DATA_SURFACE_ID_LEN,
                "ux plugin {:?} declares the data surface {surface:?}; a surface id is non-empty, \
                 has no whitespace and is at most {MAX_UX_DATA_SURFACE_ID_LEN} characters",
                self.name
            );
            anyhow::ensure!(
                surfaces.insert(surface),
                "ux plugin {:?} declares the data surface {surface:?} twice",
                self.name
            );
            anyhow::ensure!(
                !declared.tools.is_empty(),
                "ux plugin {:?} declares the data surface {surface:?} with no tools; name the \
                 tools its pages may run",
                self.name
            );
            let mut tools = BTreeSet::new();
            for tool in &declared.tools {
                anyhow::ensure!(
                    is_valid_ux_data_tool_name(tool),
                    "ux plugin {:?} declares the tool {tool:?} on data surface {surface:?}; a tool \
                     name is lowercase letters, digits and underscores, starting with a letter, \
                     at most 64 characters",
                    self.name
                );
                anyhow::ensure!(
                    tools.insert(tool.as_str()),
                    "ux plugin {:?} declares the tool {tool:?} on data surface {surface:?} twice",
                    self.name
                );
            }
        }
        Ok(())
    }
}

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

pub const PLUGIN_ARTIFACT_PREFIX: &str = "plugins/";

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
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.host_imports.is_empty() || self.job.0,
            "host_imports needs job: true; the host runs a call's script only as a job step"
        );
        anyhow::ensure!(
            self.host_imports.is_empty()
                || self.input_schema.pointer("/properties/script").is_some(),
            "host_imports: run_script needs a `script` property in input_schema"
        );
        if let Some(service) = &self.service {
            anyhow::ensure!(
                crate::plugin::authority::declared_manifold(self)?.child_process,
                "plugin service requires child_process authority"
            );
            anyhow::ensure!(
                !service.argv.is_empty()
                    && service.argv.iter().all(|arg| !arg.contains('\0'))
                    && !service.argv[0].is_empty(),
                "plugin service needs a nonempty argv without NUL bytes"
            );
            anyhow::ensure!(
                (1..=300).contains(&service.startup_timeout_secs),
                "service startup_timeout_secs must be 1..300"
            );
            anyhow::ensure!(
                self.input_schema["properties"].get("service").is_none(),
                "service is a reserved host-filled plugin input"
            );
        }
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
            let listen = manifold.get("listen");
            anyhow::ensure!(
                listen.is_none_or(|value| value == "None" || value == &serde_json::json!("None")),
                "plugin {:?} asks to listen on a port; a pack's plugins are called, not served",
                self.name
            );
            crate::plugin::http_calls::validate_declared(
                &crate::plugin::authority::declared_manifold(self)?,
            )
            .map_err(|why| {
                anyhow::anyhow!(
                    "plugin {:?} declares a network grant the host cannot serve: {why}",
                    self.name
                )
            })?;
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
            anyhow::ensure!(
                bind_dir.write_fields.is_empty() || bind_dir.access == BindAccess::ReadWrite,
                "plugin {:?} declares bind_dir.write_fields but bind_dir.access is read; a \
                 call that sets one would write, so declare access read_write",
                self.name
            );
            for field in &bind_dir.write_fields {
                anyhow::ensure!(
                    field != &bind_dir.input_field
                        && Some(field) != bind_dir.original_field.as_ref()
                        && schema_declares_property(&self.input_schema, field, |_| true),
                    "plugin {:?} bind_dir.write_fields names {field:?}, which must be a property \
                     of input_schema other than the bound path fields",
                    self.name
                );
            }
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
        self.validate_job_manifest_fields()
    }

    /// The static rules for the `data` field;
    /// checks against the archive itself belong to install.
    fn validate_job_manifest_fields(&self) -> Result<()> {
        let name = &self.name;
        if let Some(data) = &self.data {
            anyhow::ensure!(
                !data.collections.is_empty(),
                "plugin {name:?} data.collections is empty; list the collections it may use or \
                 drop data"
            );
            let mut seen = BTreeSet::new();
            for collection in &data.collections {
                anyhow::ensure!(
                    seen.insert(collection),
                    "plugin {name:?} data.collections lists {collection:?} twice; list each once"
                );
            }
            for op in &data.ops {
                anyhow::ensure!(
                    DATA_OPS.contains(&op.as_str()),
                    "plugin {name:?} data.ops has unknown op {op:?}; use {}",
                    DATA_OPS.join(", ")
                );
            }
            for field in data.stamped.keys() {
                crate::graphql::validate_graphql_name(field).with_context(|| {
                    format!(
                        "plugin {name:?} data.stamped key {field:?} must be a GraphQL field name"
                    )
                })?;
                anyhow::ensure!(
                    !crate::toolset::is_secret_env_name(field),
                    "plugin {name:?} data.stamped field {field:?} looks like a secret name; \
                     rename the field"
                );
            }
        }
        Ok(())
    }
}

/// The operations `data.ops` may list.
const DATA_OPS: &[&str] = &["query", "insert", "update", "delete"];

fn limit_range_reason(value: u32, ceiling: u32, unit: &str) -> String {
    if value == 0 {
        "must be at least 1".to_owned()
    } else {
        format!("must be at most the host ceiling of {ceiling} {unit}")
    }
}

fn schema_declares_string_property(input_schema: &serde_json::Value, field: &str) -> bool {
    schema_declares_property(input_schema, field, |property| {
        property.get("type").and_then(serde_json::Value::as_str) == Some("string")
    })
}

fn schema_declares_property(
    input_schema: &serde_json::Value,
    field: &str,
    accept: impl Fn(&serde_json::Value) -> bool,
) -> bool {
    let declares = |schema: &serde_json::Value| {
        schema
            .get("properties")
            .and_then(|properties| properties.get(field))
            .is_some_and(&accept)
    };
    declares(input_schema)
        || input_schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|branches| branches.iter().any(declares))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub schemas: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub external_dependencies: Vec<PackageExternalDependency>,
}

impl PackManifest {
    /// The one manifest decode. A retired `graph` kind is refused before the
    /// typed decode so the error names the next step instead of serde's
    /// unknown-variant message.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let raw: serde_json::Value = serde_json::from_slice(bytes)?;
        if raw.get("kind").and_then(serde_json::Value::as_str) == Some("graph") {
            let text = |key: &str, fallback: &str| {
                raw.get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(fallback)
                    .to_owned()
            };
            anyhow::bail!(
                "graph packs are retired; run `gents pack remove {}/{}` and install the 2.x version",
                text("namespace", crate::pack_archive::DEFAULT_NAMESPACE),
                text("name", "<name>")
            );
        }
        Ok(serde_json::from_value(raw)?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageExternalDependency {
    pub service_id: String,
    pub description: String,
    pub repository_url: String,
    pub install_command: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInstallOptions {
    pub node_did: String,
}

pub fn is_valid_pack_name(name: &str) -> bool {
    asset_path::is_snake_case_name(name)
}

pub fn is_distributable_asset_path(path: &str) -> bool {
    asset_path::is_distributable_asset(path) && asset_path::has_canonical_asset_spelling(path)
}

pub(crate) fn open_pack_asset(root: &std::path::Path, path: &str) -> Result<std::fs::File> {
    asset_path::open_pack_asset(root, path)
}

pub fn validate_manifest(name: &str, manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1 && manifest.name == name,
        "invalid pack identity/version"
    );
    validate_pack_manifest(manifest)
}

pub fn validate_pack_manifest(manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1,
        "unsupported pack manifest version"
    );
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents || manifest.metadata.dependencies.is_empty(),
        "only document packs support package dependencies; nested dependencies are unsupported"
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

    let mut ux_names = BTreeSet::new();
    for ux in &manifest.metadata.ux {
        ux.validate(&plugin_names)?;
        anyhow::ensure!(
            ux_names.insert(ux.name.as_str()),
            "pack declares the ux plugin {:?} twice",
            ux.name
        );
        // A UX plugin's files travel as declared assets like a plugin's
        // artifact, so the pack digest covers the code the webview runs.
        for (what, path) in [("entry", &ux.entry), ("css", &ux.css)] {
            if let Some(path) = path {
                anyhow::ensure!(
                    unique.contains(path),
                    "pack declares the ux plugin {:?} but not its {what} {path:?} as an asset",
                    ux.name
                );
            }
        }
    }

    let mut slot_names = BTreeSet::new();
    let mut slot_agents = BTreeSet::new();
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
            slot.optional == slot.agents.is_empty(),
            "inference slot {:?} must name at least one agent, unless it is optional, and \
             an optional slot names none",
            slot.name
        );
        let mut local = BTreeSet::new();
        for agent in &slot.agents {
            anyhow::ensure!(
                !agent.trim().is_empty() && local.insert(agent.as_str()),
                "inference slot {:?} repeats or has a blank agent",
                slot.name
            );
            anyhow::ensure!(
                slot_agents.insert(agent.as_str()),
                "agent {agent:?} belongs to more than one inference slot"
            );
        }
    }
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents
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
                    declared.name == *slot && declared.optional && declared.agents.is_empty()
                }),
                "plugin {:?} model slot {slot:?} must be optional and have no agents",
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
        PackKind::Documents => {
            let config = manifest
                .config
                .as_deref()
                .context("document pack requires a config asset")?;
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
    Ok(())
}

pub fn declared_paths(manifest: &PackManifest) -> Vec<String> {
    let mut paths = manifest.metadata.assets.clone();
    paths.push("manifest.json".to_owned());
    paths.sort();
    paths.dedup();
    paths
}

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

#[derive(Default)]
pub struct PackDigester(sha2::Sha256);

impl PackDigester {
    pub fn begin_entry(&mut self, path: &str, len: u64) {
        use sha2::Digest;
        self.0.update((path.len() as u64).to_be_bytes());
        self.0.update(path.as_bytes());
        self.0.update(len.to_be_bytes());
    }

    pub fn update(&mut self, bytes: &[u8]) {
        sha2::Digest::update(&mut self.0, bytes);
    }

    pub fn finish(self) -> String {
        format!("sha256:{:x}", sha2::Digest::finalize(self.0))
    }
}

#[cfg(test)]
mod tests;
