use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{is_distributable_asset_path, is_valid_pack_name};

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
