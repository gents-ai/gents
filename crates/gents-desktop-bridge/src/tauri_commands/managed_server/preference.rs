use super::authority::{parse_tool_ceiling, EffectiveManagedAuthority, HomeIdentity};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::ManagedServerToolCeiling;
use gents_desktop_core::preference::{ReviewedHome, StoredManagedServer, MANAGED_SERVER_CONFIG};

pub(super) async fn read_initialized_did(node_home: &std::path::Path) -> Option<String> {
    tokio::fs::read(node_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

pub(super) async fn read_initialized_name(node_home: &std::path::Path) -> Option<String> {
    tokio::fs::read(node_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

pub(super) async fn refuse_renaming_home(
    node_home: &std::path::Path,
    requested: &str,
) -> Result<(), BridgeError> {
    if !gents_server::server_host::initialized_home(node_home) {
        return Ok(());
    }
    let initialized = read_initialized_name(node_home).await;
    if name_confirmed_by_home(requested, initialized.as_deref()) {
        return Ok(());
    }
    let requested = requested.trim();
    Err(BridgeError::new(
        BridgeErrorCode::InvalidArgument,
        match initialized {
            Some(name) => format!(
                "This computer already has a local Node named {name}, so {requested} was not created. Go back to continue with {name}."
            ),
            None => format!(
                "The local node home at {} has no readable node name; it was left unchanged.",
                node_home.display()
            ),
        },
    ))
}

fn name_confirmed_by_home(requested: &str, initialized: Option<&str>) -> bool {
    initialized.is_some_and(|name| name.trim() == requested.trim())
}

pub(super) async fn save_confirmed_preference(
    state: &DesktopAppState,
    node_home: &std::path::Path,
    stored: &StoredManagedServer,
) -> Result<(), BridgeError> {
    if name_confirmed_by_home(
        &stored.node_name,
        read_initialized_name(node_home).await.as_deref(),
    ) {
        let reviewed_for = read_home_identity(node_home)
            .await
            .map(|identity| identity.reviewed);
        save_preference(
            state,
            &StoredManagedServer {
                reviewed_for,
                ..stored.clone()
            },
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn read_home_identity(node_home: &std::path::Path) -> Option<HomeIdentity> {
    let home = tokio::fs::canonicalize(node_home).await.ok()?;
    let bytes = tokio::fs::read(node_home.join("init.json")).await.ok()?;
    let init: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let text = |key: &str| {
        init.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(HomeIdentity {
        reviewed: ReviewedHome {
            node_home: home.to_string_lossy().into_owned(),
            node_did: text("node_did")?,
        },
        tool_ceiling: text("tool_ceiling").as_deref().and_then(parse_tool_ceiling),
        tool_root: text("tool_root"),
    })
}

fn bind_preference(
    mut stored: StoredManagedServer,
    home: Option<&HomeIdentity>,
) -> StoredManagedServer {
    let bound = home.is_some_and(|home| {
        stored.reviewed_for.as_ref() == Some(&home.reviewed)
            && stored.tool_ceiling.is_some()
            && home.tool_ceiling == stored.tool_ceiling
            && home.tool_root == stored.tool_root
    });
    if !bound {
        stored.tool_ceiling = None;
        stored.tool_root = None;
    }
    stored
}

pub(super) fn start_authority(
    tool_ceiling: Option<ManagedServerToolCeiling>,
    tool_root: Option<&str>,
    bound: Option<&StoredManagedServer>,
) -> Result<EffectiveManagedAuthority, BridgeError> {
    match tool_ceiling {
        Some(ceiling) => EffectiveManagedAuthority::from_request(ceiling, tool_root),
        None => match bound.and_then(|stored| {
            stored
                .tool_ceiling
                .map(|ceiling| (ceiling, stored.tool_root.as_deref()))
        }) {
            Some((ceiling, root)) => EffectiveManagedAuthority::from_request(ceiling, root),
            None => Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "Review host access for this node home before starting it.",
            )),
        },
    }
}

pub(super) async fn load_bound_preference(
    state: &DesktopAppState,
) -> Result<Option<StoredManagedServer>, BridgeError> {
    let Some(stored) = load_preference(state).await? else {
        return Ok(None);
    };
    let home = match state.policy.node_home.as_deref() {
        Some(node_home) => read_home_identity(node_home).await,
        None => None,
    };
    Ok(Some(bind_preference(stored, home.as_ref())))
}

pub(super) async fn load_preference(
    state: &DesktopAppState,
) -> Result<Option<StoredManagedServer>, BridgeError> {
    let path = state
        .policy
        .desktop_paths
        .root()
        .join(MANAGED_SERVER_CONFIG);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| BridgeError::untyped(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BridgeError::untyped(error.to_string())),
    }
}

pub(super) async fn save_preference(
    state: &DesktopAppState,
    stored: &StoredManagedServer,
) -> Result<(), BridgeError> {
    state
        .policy
        .desktop_paths
        .ensure_root_dirs()
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let path = state
        .policy
        .desktop_paths
        .root()
        .join(MANAGED_SERVER_CONFIG);
    let bytes = serde_json::to_vec_pretty(stored)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
