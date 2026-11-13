//! The Engineer's core packs and the plugin interface version every pack
//! archive is checked against (Lean `Pack.CorePacks`). A core pack is built
//! with the runtime and carried by its release; init installs it from there,
//! without the registry and without asking.

use serde::{Deserialize, Serialize};

/// Lean `Pack.CorePacks.CorePack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorePack {
    PackBuilder,
    Web,
    DynamicWorkflows,
    Charts,
    DataTables,
    Script,
}

impl CorePack {
    /// Lean `Pack.CorePacks.all`, in the order init installs them.
    pub const ALL: [Self; 6] = [
        Self::PackBuilder,
        Self::Web,
        Self::DynamicWorkflows,
        Self::Charts,
        Self::DataTables,
        Self::Script,
    ];

    pub const fn coordinate(self) -> &'static str {
        match self {
            Self::PackBuilder => "gents/pack_builder",
            Self::Web => "gents/web",
            Self::DynamicWorkflows => "gents/dynamic_workflows",
            Self::Charts => "gents/charts",
            Self::DataTables => "gents/data_tables",
            Self::Script => "gents/script",
        }
    }

    /// The core pack a coordinate names, written `ns/name` or `name`.
    pub fn of_coordinate(coordinate: &str) -> Option<Self> {
        let (namespace, name) = crate::pack::registry::split_pack_coordinate(coordinate);
        Self::ALL
            .into_iter()
            .find(|pack| pack.coordinate() == format!("{namespace}/{name}"))
    }
}

/// Lean `Pack.CorePacks.Origin`: where the archive an install takes comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Carried by this runtime's release.
    Release,
    /// The registry, a home's store, a file or a directory.
    Other,
}

/// Lean `Pack.CorePacks.approvalRequired`.
pub fn approval_required(origin: Origin, grants: bool) -> bool {
    grants && origin == Origin::Other
}

/// Lean `Pack.CorePacks.Wiring`: what one core pack leaves on the Engineer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wiring {
    pub wired: bool,
    pub tool_calls: bool,
    pub consent_dynamic: bool,
    pub root_readers: bool,
    pub agents_active: bool,
}

/// Lean `Pack.CorePacks.initWiring`.
pub fn init_wiring(pack: CorePack) -> Wiring {
    match pack {
        CorePack::DynamicWorkflows => Wiring {
            wired: false,
            tool_calls: false,
            consent_dynamic: false,
            root_readers: false,
            agents_active: false,
        },
        CorePack::Script => Wiring {
            wired: true,
            tool_calls: true,
            consent_dynamic: false,
            root_readers: true,
            agents_active: true,
        },
        _ => Wiring {
            wired: true,
            tool_calls: false,
            consent_dynamic: false,
            root_readers: true,
            agents_active: true,
        },
    }
}

/// Lean `Pack.CorePacks.enabledWiring`: what the user's explicit enable
/// operation leaves.
pub fn enabled_wiring(pack: CorePack) -> Wiring {
    match pack {
        CorePack::DynamicWorkflows => Wiring {
            wired: true,
            tool_calls: false,
            consent_dynamic: true,
            root_readers: true,
            agents_active: true,
        },
        pack => init_wiring(pack),
    }
}

/// Lean `Pack.CorePacks.EnableRoute`: where the enable operation writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnableRoute {
    /// No runtime serves the home: written through its own store.
    Home,
    /// Written through the runtime serving the home.
    Runtime,
    /// A pack the home lacks is installed as the Engineer from the home's
    /// own store, which a serving runtime holds: refused before anything is
    /// written.
    StopFirst,
}

/// Lean `Pack.CorePacks.enableRoute`.
pub fn enable_route(served: bool, installed: bool) -> EnableRoute {
    if !served {
        EnableRoute::Home
    } else if installed {
        EnableRoute::Runtime
    } else {
        EnableRoute::StopFirst
    }
}

/// Lean `Pack.CorePacks.currentInterface`: the plugin interface this runtime
/// implements. A pack manifest's `plugin_interface` declares the one it was
/// built for.
pub const PLUGIN_INTERFACE: u32 = 1;

/// Lean `Pack.CorePacks.oldestInterface`.
pub const OLDEST_PLUGIN_INTERFACE: u32 = 1;

/// Lean `Pack.CorePacks.builtFor`: a manifest that declares none was built
/// before the interface had a version.
pub fn built_for(declared: Option<u32>) -> u32 {
    declared.unwrap_or(1)
}

/// Lean `Pack.CorePacks.InterfaceAdmission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceAdmission {
    Compatible,
    TooOld,
    TooNew,
}

/// Lean `Pack.CorePacks.interfaceAdmission`.
pub fn interface_admission(declared: Option<u32>) -> InterfaceAdmission {
    let version = built_for(declared);
    if version < OLDEST_PLUGIN_INTERFACE {
        InterfaceAdmission::TooOld
    } else if PLUGIN_INTERFACE < version {
        InterfaceAdmission::TooNew
    } else {
        InterfaceAdmission::Compatible
    }
}

/// Lean `Pack.CorePacks.UpdateDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateDecision {
    Take,
    KeepInstalled,
}

/// Lean `Pack.CorePacks.updateDecision`.
pub fn update_decision(candidate: InterfaceAdmission) -> UpdateDecision {
    if candidate == InterfaceAdmission::Compatible {
        UpdateDecision::Take
    } else {
        UpdateDecision::KeepInstalled
    }
}

/// A pack this runtime does not load because of the plugin interface it was
/// built for. The message names the next call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginInterfaceError {
    #[error(
        "{coordinate} {version} is built for plugin interface {declared}, which this runtime no longer loads (it loads {OLDEST_PLUGIN_INTERFACE} through {PLUGIN_INTERFACE}). Next: install a newer build with `gents config pack install {coordinate}`"
    )]
    TooOld {
        coordinate: String,
        version: String,
        declared: u32,
    },
    #[error(
        "{coordinate} {version} is built for plugin interface {declared}, newer than this runtime loads ({OLDEST_PLUGIN_INTERFACE} through {PLUGIN_INTERFACE}). Next: update gents, or install a build for this runtime with `gents config pack install {coordinate}@<version>` (`gents config pack info {coordinate}` lists the versions)"
    )]
    TooNew {
        coordinate: String,
        version: String,
        declared: u32,
    },
}

impl PluginInterfaceError {
    pub fn admission(&self) -> InterfaceAdmission {
        match self {
            Self::TooOld { .. } => InterfaceAdmission::TooOld,
            Self::TooNew { .. } => InterfaceAdmission::TooNew,
        }
    }
}

/// The interface refusal in `error`'s chain: an archive open erases it into
/// `anyhow`, and an install or update decides on it.
pub fn plugin_interface_refusal<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a PluginInterfaceError> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause.downcast_ref::<PluginInterfaceError>()
    })
}

/// An installed plugin whose owning pack was built for a plugin interface
/// this runtime does not load. The message names the next call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "plugin {plugin} was installed from {pack}, which is built for plugin interface {declared}; this runtime loads {OLDEST_PLUGIN_INTERFACE} through {PLUGIN_INTERFACE}, so it does not run it. Next: `gents config pack update {pack}`, or `gents config pack install {pack}` for a build this runtime loads"
)]
pub struct InstalledInterfaceError {
    pub plugin: String,
    pub pack: String,
    pub declared: u32,
    pub admission: InterfaceAdmission,
}

/// Refuses to run plugin `plugin`, installed from `pack` built for
/// `declared`, when this runtime does not load that interface.
pub fn check_installed_interface(
    plugin: &str,
    pack: &str,
    declared: Option<u32>,
) -> Result<(), InstalledInterfaceError> {
    match interface_admission(declared) {
        InterfaceAdmission::Compatible => Ok(()),
        admission => Err(InstalledInterfaceError {
            plugin: plugin.to_owned(),
            pack: pack.to_owned(),
            declared: built_for(declared),
            admission,
        }),
    }
}

/// Refuses `manifest` when this runtime does not load the interface it was
/// built for.
pub fn check_plugin_interface(
    manifest: &crate::pack::PackManifest,
) -> Result<(), PluginInterfaceError> {
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let declared = built_for(manifest.plugin_interface);
    match interface_admission(manifest.plugin_interface) {
        InterfaceAdmission::Compatible => Ok(()),
        InterfaceAdmission::TooOld => Err(PluginInterfaceError::TooOld {
            coordinate,
            version: manifest.version.clone(),
            declared,
        }),
        InterfaceAdmission::TooNew => Err(PluginInterfaceError::TooNew {
            coordinate,
            version: manifest.version.clone(),
            declared,
        }),
    }
}

#[cfg(test)]
mod conformance {
    use gents_lean_vocab::families;

    use super::*;

    #[test]
    fn approval_required_matches_the_lean_model() {
        families::pack_core_packs_approval_required::replay(|input| {
            approval_required(input.origin, input.grants)
        });
    }

    #[test]
    fn enable_route_matches_the_lean_model() {
        families::pack_core_packs_enable_route::replay(|input| {
            enable_route(input.served, input.installed)
        });
    }
}
