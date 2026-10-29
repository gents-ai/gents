use crate::error::{BridgeError, BridgeErrorCode};
use crate::types::{ManagedServerStatus, ManagedServerToolCeiling};
use gents_desktop_core::preference::ReviewedHome;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeIdentity {
    pub(super) reviewed: ReviewedHome,
    pub(super) tool_ceiling: Option<ManagedServerToolCeiling>,
    pub(super) tool_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EffectiveManagedAuthority {
    pub(super) tool_ceiling: ManagedServerToolCeiling,
    pub(super) tool_root: Option<PathBuf>,
}

impl EffectiveManagedAuthority {
    pub(super) fn from_request(
        tool_ceiling: ManagedServerToolCeiling,
        tool_root: Option<&str>,
    ) -> Result<Self, BridgeError> {
        let tool_root = match tool_ceiling {
            ManagedServerToolCeiling::MetaOnly => {
                if tool_root.is_some_and(|root| !root.trim().is_empty()) {
                    return Err(BridgeError::new(
                        BridgeErrorCode::InvalidArgument,
                        "toolRoot must be omitted when toolCeiling is meta-only",
                    ));
                }
                None
            }
            ManagedServerToolCeiling::Readonly | ManagedServerToolCeiling::Readwrite => {
                let root = tool_root
                    .filter(|root| !root.trim().is_empty())
                    .ok_or_else(|| {
                        BridgeError::new(
                            BridgeErrorCode::InvalidArgument,
                            "toolRoot is required when filesystem access is enabled",
                        )
                    })?;
                Some(validate_tool_root(Path::new(root))?)
            }
        };
        Ok(Self {
            tool_ceiling,
            tool_root,
        })
    }

    pub(super) fn stored(&self) -> (ManagedServerToolCeiling, Option<String>) {
        (
            self.tool_ceiling,
            self.tool_root
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        )
    }
}

pub(super) fn validate_tool_root(path: &Path) -> Result<PathBuf, BridgeError> {
    if !path.is_absolute() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Choose an absolute directory path.",
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("Cannot access {}: {error}", path.display()),
        )
    })?;
    if !canonical.is_dir() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("{} is not a directory.", canonical.display()),
        ));
    }
    if canonical.parent().is_none() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The filesystem root is too broad. Choose your home directory or a folder inside it.",
        ));
    }
    if let Some(home) = dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok()) {
        if home.starts_with(&canonical) && home != canonical {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                format!(
                    "{} is broader than your home directory. Choose your home directory or a more specific folder.",
                    canonical.display()
                ),
            ));
        }
    }
    std::fs::read_dir(&canonical).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("Cannot read {}: {error}", canonical.display()),
        )
    })?;
    Ok(canonical)
}

pub(super) fn runtime_tool_ceiling(
    value: ManagedServerToolCeiling,
) -> gents_server::server_host::ManagedToolCeiling {
    use gents_server::server_host::ManagedToolCeiling;
    match value {
        ManagedServerToolCeiling::MetaOnly => ManagedToolCeiling::MetaOnly,
        ManagedServerToolCeiling::Readonly => ManagedToolCeiling::Readonly,
        ManagedServerToolCeiling::Readwrite => ManagedToolCeiling::Readwrite,
    }
}

pub(super) fn validate_ready_runtime(
    status: &ManagedServerStatus,
    authority: &EffectiveManagedAuthority,
    node_home: &Path,
) -> anyhow::Result<()> {
    let expected_did = std::fs::read(node_home.join("init.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .filter(|did| !did.trim().is_empty())
                .map(str::to_owned)
        });
    let expected_root = authority
        .tool_root
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    if expected_did.is_none()
        || status.node_did.as_deref() != expected_did.as_deref()
        || status.effective_tool_ceiling != Some(authority.tool_ceiling)
        || status.effective_tool_root.as_deref() != expected_root.as_deref()
    {
        anyhow::bail!(
            "native runtime readiness did not match the initialized identity and reviewed host authority (live did={:?} ceiling={:?} root={:?}; expected did={:?} ceiling={:?} root={:?})",
            status.node_did,
            status.effective_tool_ceiling,
            status.effective_tool_root,
            expected_did,
            Some(authority.tool_ceiling),
            expected_root
        );
    }
    Ok(())
}

pub(super) fn suggested_tool_root() -> Option<String> {
    dirs::home_dir()
        .and_then(|path| std::fs::canonicalize(path).ok())
        .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn parse_tool_ceiling(value: &str) -> Option<ManagedServerToolCeiling> {
    match value.trim().to_ascii_lowercase().as_str() {
        "meta-only" | "metaonly" | "meta_only" => Some(ManagedServerToolCeiling::MetaOnly),
        "readonly" => Some(ManagedServerToolCeiling::Readonly),
        "readwrite" => Some(ManagedServerToolCeiling::Readwrite),
        _ => None,
    }
}
