use super::shared::wire_enum;
use serde::Deserialize;
use serde::Serialize;
use std::num::NonZeroUsize;
use std::path::PathBuf;

wire_enum! {
    pub enum NetworkApprovalProtocol {
        Http,
        Https,
        Socks5Tcp,
        Socks5Udp,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: NetworkApprovalProtocol,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalFileSystemPermissions {
    /// This will be removed in favor of `entries`.
    pub read: Option<Vec<AbsolutePathBuf>>,
    /// This will be removed in favor of `entries`.
    pub write: Option<Vec<AbsolutePathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob_scan_max_depth: Option<NonZeroUsize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FileSystemSandboxEntry>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalNetworkPermissions {
    pub enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct RequestPermissionProfile {
    pub network: Option<AdditionalNetworkPermissions>,
    pub file_system: Option<AdditionalFileSystemPermissions>,
}

wire_enum!(
    pub enum FileSystemAccessMode {
        Read,
        Write,
        Deny,
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileSystemSpecialPath {
    Root,
    Minimal,
    #[serde(alias = "current_working_directory")]
    ProjectRoots {
        subpath: Option<PathBuf>,
    },
    Tmpdir,
    SlashTmp,
    Unknown {
        path: String,
        subpath: Option<PathBuf>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileSystemPath {
    Path { path: AbsolutePathBuf },
    GlobPattern { pattern: String },
    Special { value: FileSystemSpecialPath },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemSandboxEntry {
    pub path: FileSystemPath,
    pub access: FileSystemAccessMode,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileListParams {
    /// Opaque pagination cursor returned by a previous call.
    pub cursor: Option<String>,
    /// Optional page size; defaults to the full result set.
    pub limit: Option<u32>,
    /// Optional working directory to resolve project config layers.
    pub cwd: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileSummary {
    /// Available permission profile identifier.
    pub id: String,
    /// Optional user-facing description for display in clients.
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfileListResponse {
    pub data: Vec<PermissionProfileSummary>,
    /// Opaque cursor to pass to the next call to continue after the last item.
    /// If None, there are no more items to return.
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivePermissionProfile {
    /// Identifier from `default_permissions` or the implicit built-in default,
    /// such as `:workspace` or a user-defined `[permissions.<id>]` profile.
    pub id: String,
    /// Parent profile identifier from the selected permissions profile's
    /// `extends` setting, when present.
    #[serde(default)]
    pub extends: Option<String>,
}

impl ActivePermissionProfile {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            extends: None,
        }
    }

    pub fn read_only() -> Self {
        Self::new(":read-only")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalPermissionProfile {
    /// Partial overlay used for per-command permission requests.
    pub network: Option<AdditionalNetworkPermissions>,
    pub file_system: Option<AdditionalFileSystemPermissions>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GrantedPermissionProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<AdditionalNetworkPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_system: Option<AdditionalFileSystemPermissions>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NetworkAccess {
    #[default]
    Restricted,
    Enabled,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SandboxPolicy {
    DangerFullAccess,
    #[serde(rename_all = "camelCase")]
    ReadOnly {
        #[serde(default)]
        network_access: bool,
    },
    #[serde(rename_all = "camelCase")]
    ExternalSandbox {
        #[serde(default)]
        network_access: NetworkAccess,
    },
    #[serde(rename_all = "camelCase")]
    WorkspaceWrite {
        #[serde(default)]
        writable_roots: Vec<AbsolutePathBuf>,
        #[serde(default)]
        network_access: bool,
        #[serde(default)]
        exclude_tmpdir_env_var: bool,
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum SandboxPolicyDeserialize {
    DangerFullAccess,
    #[serde(rename_all = "camelCase")]
    ReadOnly {
        #[serde(default)]
        network_access: bool,
        #[serde(default)]
        access: Option<LegacyReadOnlyAccess>,
    },
    #[serde(rename_all = "camelCase")]
    ExternalSandbox {
        #[serde(default)]
        network_access: NetworkAccess,
    },
    #[serde(rename_all = "camelCase")]
    WorkspaceWrite {
        #[serde(default)]
        writable_roots: Vec<AbsolutePathBuf>,
        #[serde(default)]
        read_only_access: Option<LegacyReadOnlyAccess>,
        #[serde(default)]
        network_access: bool,
        #[serde(default)]
        exclude_tmpdir_env_var: bool,
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum LegacyReadOnlyAccess {
    FullAccess,
    Restricted,
}

impl<'de> Deserialize<'de> for SandboxPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match SandboxPolicyDeserialize::deserialize(deserializer)? {
            SandboxPolicyDeserialize::DangerFullAccess => Ok(SandboxPolicy::DangerFullAccess),
            SandboxPolicyDeserialize::ReadOnly {
                network_access,
                access,
            } => {
                if matches!(access, Some(LegacyReadOnlyAccess::Restricted)) {
                    return Err(serde::de::Error::custom(
                        "readOnly.access is no longer supported; use permissionProfile for restricted reads",
                    ));
                }
                Ok(SandboxPolicy::ReadOnly { network_access })
            }
            SandboxPolicyDeserialize::ExternalSandbox { network_access } => {
                Ok(SandboxPolicy::ExternalSandbox { network_access })
            }
            SandboxPolicyDeserialize::WorkspaceWrite {
                writable_roots,
                read_only_access,
                network_access,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
            } => {
                if matches!(read_only_access, Some(LegacyReadOnlyAccess::Restricted)) {
                    return Err(serde::de::Error::custom(
                        "workspaceWrite.readOnlyAccess is no longer supported; use permissionProfile for restricted reads",
                    ));
                }
                Ok(SandboxPolicy::WorkspaceWrite {
                    writable_roots,
                    network_access,
                    exclude_tmpdir_env_var,
                    exclude_slash_tmp,
                })
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(transparent)]
pub struct ExecPolicyAmendment {
    pub command: Vec<String>,
}

wire_enum!(
    pub enum NetworkPolicyRuleAction {
        Allow,
        Deny,
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyRuleAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    /// Unix timestamp (in milliseconds) when this approval request started.
    pub started_at_ms: i64,
    pub cwd: AbsolutePathBuf,
    pub reason: Option<String>,
    pub permissions: RequestPermissionProfile,
}

wire_enum!(
    #[derive(Default)]
    pub enum PermissionGrantScope {
        #[default]
        Turn,
        Session,
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsRequestApprovalResponse {
    pub permissions: GrantedPermissionProfile,
    #[serde(default)]
    pub scope: PermissionGrantScope,
    /// Review every subsequent command in this turn before normal sandboxed execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_auto_review: Option<bool>,
}
use crate::core_types::AbsolutePathBuf;
