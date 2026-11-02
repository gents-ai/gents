use anyhow::{Context, Result};
use gents_protocol::job::{PluginDataDeclaration, PluginJobDeclaration};
use serde::{Deserialize, Serialize};

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
