use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod archive;
pub mod authoring;
pub mod home_install;
mod inference;
mod installation;
pub mod interpolate;
mod loader;
pub mod local;
mod manifest_validation;
pub mod materialize;
mod plugin;
pub mod plugin_lifecycle;
mod plugin_validation;
mod provenance;
#[cfg(test)]
mod publication_contract_tests;
pub mod registry;
pub mod resolve;
pub mod store;
mod ux;
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
pub use manifest_validation::{validate_manifest, validate_pack_manifest};
pub use plugin::{
    tool_instructions, BindAccess, HostImport, PackPlugin, PluginDirBinding, PluginLimits,
    PluginService, MAX_TOOL_INSTRUCTIONS_BYTES, PLUGIN_ARTIFACT_PREFIX, SUPPORTED_PLUGIN_LANGUAGES,
};
pub(crate) use provenance::pack_artifact_document_digest;
pub use provenance::{pack_document_digests, pack_origin_from_tags, pack_origin_tag};
pub use ux::{
    is_valid_directive_name, is_valid_ux_data_tool_name, PackUxPlugin, UxContributions,
    UxDataSurface, MAX_UX_DATA_SURFACE_ID_LEN, UX_PLUGIN_AREAS, UX_PLUGIN_PREFIX,
};

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
        deserialize_with = "crate::documents::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    pub assets: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::documents::deserialize_default_on_null",
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
    crate::pack::archive::DEFAULT_NAMESPACE.to_owned()
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
        deserialize_with = "crate::documents::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub schemas: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::documents::deserialize_default_on_null",
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
                text("namespace", crate::pack::archive::DEFAULT_NAMESPACE),
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
