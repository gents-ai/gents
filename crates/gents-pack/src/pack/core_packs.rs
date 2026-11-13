//! The Engineer's core packs and the Gents release, the plugin interface
//! version, every pack archive is checked against (Lean `Pack.CorePacks`).
//! A core pack is built with the runtime and carried by its release; init
//! installs it from there, without the registry and without asking.

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
}

/// Lean `Pack.CorePacks.initWiring`.
pub fn init_wiring(pack: CorePack) -> Wiring {
    match pack {
        CorePack::DynamicWorkflows => Wiring {
            wired: false,
            tool_calls: false,
            consent_dynamic: false,
            root_readers: false,
        },
        CorePack::Script => Wiring {
            wired: true,
            tool_calls: true,
            consent_dynamic: false,
            root_readers: true,
        },
        _ => Wiring {
            wired: true,
            tool_calls: false,
            consent_dynamic: false,
            root_readers: true,
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
        },
        pack => init_wiring(pack),
    }
}

/// Lean `Pack.CorePacks.Release`: a Gents release, written `MAJOR.MINOR.PATCH`
/// or, for a release candidate, `MAJOR.MINOR.PATCH-rcN`. The plugin interface
/// version is the release: a pack manifest's `gents_version` names the one it
/// was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Release {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub rc: Option<u64>,
}

/// A version that is not written as a Gents release.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{text:?} is not a Gents release; a release is written MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-rcN, such as 1.0.0 or 1.0.0-rc2"
)]
pub struct ReleaseParseError {
    pub text: String,
}

impl Release {
    /// Lean `Pack.CorePacks.baseline`.
    pub const BASELINE: Self = Self {
        major: 1,
        minor: 0,
        patch: 0,
        rc: Some(2),
    };

    /// Reads exactly `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-rcN`, each a
    /// decimal number without a leading zero.
    pub fn parse(text: &str) -> Result<Self, ReleaseParseError> {
        let refused = || ReleaseParseError {
            text: text.to_owned(),
        };
        let (triple, rc) = match text.split_once('-') {
            Some((triple, pre)) => (triple, Some(pre.strip_prefix("rc").ok_or_else(refused)?)),
            None => (text, None),
        };
        let mut parts = triple.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(refused());
        };
        let number = |part: &str| release_number(part).ok_or_else(refused);
        Ok(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
            rc: rc.map(number).transpose()?,
        })
    }

    /// Lean `Pack.CorePacks.Release.le`: release precedence, with the
    /// candidates of one triple in numeric order before its release.
    pub fn le(&self, other: &Self) -> bool {
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => match (self.rc, other.rc) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(own), Some(theirs)) => own <= theirs,
            },
        }
    }
}

fn release_number(part: &str) -> Option<u64> {
    let canonical = !part.is_empty()
        && part.bytes().all(|byte| byte.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'));
    if canonical {
        part.parse().ok()
    } else {
        None
    }
}

impl std::fmt::Display for Release {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match self.rc {
            Some(rc) => write!(f, "-rc{rc}"),
            None => Ok(()),
        }
    }
}

impl std::str::FromStr for Release {
    type Err = ReleaseParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl Serialize for Release {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Release {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// The release this runtime is: the workspace version it was built from.
pub fn runtime_release() -> Release {
    Release::parse(env!("CARGO_PKG_VERSION"))
        .expect("the workspace version is written as a Gents release")
}

/// Lean `Pack.CorePacks.builtFor`.
pub fn built_for(declared: Option<&Release>) -> Release {
    declared.copied().unwrap_or(Release::BASELINE)
}

/// Lean `Pack.CorePacks.InterfaceAdmission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceAdmission {
    Compatible,
    OlderMajor,
    TooNew,
}

/// Lean `Pack.CorePacks.interfaceAdmission`.
pub fn interface_admission(runtime: &Release, declared: Option<&Release>) -> InterfaceAdmission {
    let built = built_for(declared);
    if built.major < runtime.major {
        InterfaceAdmission::OlderMajor
    } else if runtime.major < built.major || !built.le(runtime) {
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

/// A pack this runtime does not load because of the Gents release it was
/// built for. The message names the next call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginInterfaceError {
    #[error(
        "{coordinate} {version} is built for gents {declared}, an earlier major version than this runtime (gents {runtime}); after a breaking release the pack needs a build for gents {major}. Next: install a newer build with `gents config pack install {coordinate}`",
        major = .runtime.major
    )]
    OlderMajor {
        coordinate: String,
        version: String,
        declared: Release,
        runtime: Release,
    },
    #[error(
        "{coordinate} {version} is built for gents {declared}, newer than this runtime (gents {runtime}). Next: update gents (`gents runtime version` shows yours), or install a build for gents {runtime} with `gents config pack install {coordinate}@<version>` (`gents config pack info {coordinate}` lists the versions)"
    )]
    TooNew {
        coordinate: String,
        version: String,
        declared: Release,
        runtime: Release,
    },
    #[error(
        "{coordinate} {version} declares gents_version: {reason}. Next: build it again with `gents config pack build`, which writes the building gents release"
    )]
    Unreadable {
        coordinate: String,
        version: String,
        reason: ReleaseParseError,
    },
}

impl PluginInterfaceError {
    /// The admission a refusal of a readable declaration stands for.
    pub fn admission(&self) -> Option<InterfaceAdmission> {
        match self {
            Self::OlderMajor { .. } => Some(InterfaceAdmission::OlderMajor),
            Self::TooNew { .. } => Some(InterfaceAdmission::TooNew),
            Self::Unreadable { .. } => None,
        }
    }

    /// The refusal of `coordinate` at `version`, built for `declared`, by
    /// `runtime`; `None` when `runtime` loads it.
    pub fn refusal(
        coordinate: &str,
        version: &str,
        runtime: &Release,
        declared: Option<&Release>,
    ) -> Option<Self> {
        let coordinate = coordinate.to_owned();
        let version = version.to_owned();
        let declared = built_for(declared);
        let runtime = *runtime;
        match interface_admission(&runtime, Some(&declared)) {
            InterfaceAdmission::Compatible => None,
            InterfaceAdmission::OlderMajor => Some(Self::OlderMajor {
                coordinate,
                version,
                declared,
                runtime,
            }),
            InterfaceAdmission::TooNew => Some(Self::TooNew {
                coordinate,
                version,
                declared,
                runtime,
            }),
        }
    }
}

/// The release `manifest` declares it was built for, if it declares one.
pub fn declared_release(
    manifest: &crate::pack::PackManifest,
) -> Result<Option<Release>, PluginInterfaceError> {
    manifest
        .gents_version
        .as_deref()
        .map(Release::parse)
        .transpose()
        .map_err(|reason| PluginInterfaceError::Unreadable {
            coordinate: format!("{}/{}", manifest.metadata.namespace, manifest.name),
            version: manifest.version.clone(),
            reason,
        })
}

/// Refuses `manifest` when `runtime` does not load the release it was built
/// for. Manifest validation and every archive open check against
/// [`runtime_release`].
pub fn check_gents_version(
    runtime: &Release,
    manifest: &crate::pack::PackManifest,
) -> Result<(), PluginInterfaceError> {
    let declared = declared_release(manifest)?;
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    match PluginInterfaceError::refusal(&coordinate, &manifest.version, runtime, declared.as_ref())
    {
        Some(refusal) => Err(refusal),
        None => Ok(()),
    }
}

/// An installed plugin whose owning pack was built for a release this runtime
/// does not load. The message names the next call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "plugin {plugin} was installed from {pack}, which is built for gents {declared}; this runtime is gents {runtime}, so it does not run it. Next: `gents config pack update {pack}`"
)]
pub struct InstalledInterfaceError {
    pub plugin: String,
    pub pack: String,
    pub declared: Release,
    pub runtime: Release,
    pub admission: InterfaceAdmission,
}

/// Refuses to run plugin `plugin`, installed from `pack` built for
/// `declared`, when `runtime` does not load that release.
pub fn check_installed_interface(
    plugin: &str,
    pack: &str,
    runtime: &Release,
    declared: Option<&Release>,
) -> Result<(), InstalledInterfaceError> {
    match interface_admission(runtime, declared) {
        InterfaceAdmission::Compatible => Ok(()),
        admission => Err(InstalledInterfaceError {
            plugin: plugin.to_owned(),
            pack: pack.to_owned(),
            declared: built_for(declared),
            runtime: *runtime,
            admission,
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

    /// Each case's declaration is written into a manifest as its text and
    /// read back by the check every archive open runs.
    #[test]
    fn interface_admission_conforms_through_the_manifest_check() {
        use families::pack_core_packs_interface_admission_cases as family;

        let release = |case: &family::Release| Release {
            major: case.major,
            minor: case.minor,
            patch: case.patch,
            rc: case.rc,
        };
        family::replay(|input| {
            let mut manifest = crate::pack::PackManifest::from_slice(
                serde_json::json!({
                    "manifest_version": 1,
                    "name": "interface_case",
                    "version": "1.0.0",
                    "description": "A manifest declaring the release it was built for",
                    "namespace": "fixture",
                    "kind": "assets",
                    "authors": ["gents-ai contributors"],
                    "assets": [],
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
            manifest.gents_version = input
                .declared
                .as_ref()
                .map(|declared| release(declared).to_string());
            match check_gents_version(&release(&input.runtime), &manifest) {
                Ok(()) => InterfaceAdmission::Compatible,
                Err(refusal) => refusal
                    .admission()
                    .unwrap_or_else(|| panic!("a modeled release was unreadable: {refusal}")),
            }
        });
    }
}
