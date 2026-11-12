use gents_tools::plugin::approval::workspace::{self, Mode, Support, WorkspaceKey};
use gents_desktop_core::client::ClientCore;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspacePermissionsRequest {
    pub node_did: String,
    pub agent_did: String,
    pub workspace_path: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspacePermissionsSaveRequest {
    pub scope: WorkspacePermissionsRequest,
    pub expected_revision: String,
    pub expected_workspace_path: String,
    pub mode: Mode,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspacePermissionsForgetCommandsRequest {
    pub scope: WorkspacePermissionsRequest,
    pub expected_revision: String,
    pub expected_workspace_path: String,
}

#[derive(Debug, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkspacePermissionsState {
    UnsupportedRuntime,
    RemoteUnavailable {
        support: Option<Support>,
    },
    Local {
        support: Support,
        mode: Option<Mode>,
        remembered_commands: u32,
        workspace_path: String,
        /// Revisions cross JavaScript as decimal strings without rounding.
        revision: String,
    },
}

struct LocalTarget {
    support: Support,
    home: std::path::PathBuf,
    key: WorkspaceKey,
    settings: Option<workspace::Settings>,
}

impl LocalTarget {
    fn view(self) -> WorkspacePermissionsState {
        WorkspacePermissionsState::Local {
            remembered_commands: self.settings.as_ref().map_or(0, |settings| {
                u32::try_from(settings.remembered_command_count()).unwrap_or(u32::MAX)
            }),
            support: self.support,
            workspace_path: self.key.path.to_string_lossy().into_owned(),
            mode: self
                .settings
                .as_ref()
                .map(|settings| settings.selection.mode),
            revision: self
                .settings
                .map_or(0, |settings| settings.selection.revision)
                .to_string(),
        }
    }
}

enum Target {
    Unavailable(WorkspacePermissionsState),
    Local(Box<LocalTarget>),
}

pub(super) async fn state(
    core: &ClientCore,
    request: WorkspacePermissionsRequest,
) -> Result<WorkspacePermissionsState, BridgeError> {
    Ok(match target(core, request).await? {
        Target::Unavailable(view) => view,
        Target::Local(local) => local.view(),
    })
}

pub(super) async fn save(
    core: &ClientCore,
    request: WorkspacePermissionsSaveRequest,
) -> Result<WorkspacePermissionsState, BridgeError> {
    let expected: u64 = request.expected_revision.parse().map_err(|_| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Reload workspace permissions before saving.",
        )
    })?;
    if expected.to_string() != request.expected_revision {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Reload workspace permissions before saving.",
        ));
    }
    let Target::Local(mut local) = target(core, request.scope).await? else {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "This runtime cannot change workspace permissions from the editor.",
        ));
    };
    if local.key.path != std::path::Path::new(&request.expected_workspace_path) {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The workspace changed. Reopen permissions before saving.",
        ));
    }
    if local
        .settings
        .as_ref()
        .map_or(0, |settings| settings.selection.revision)
        != expected
    {
        return Err(super::backend_error(
            workspace::SelectionError::Stale.into(),
        ));
    }
    let reviewer = if request.mode == Mode::AutoReview {
        // Configuring the Task may use only the existing signer cache; opening
        // the permission picker must not unlock a runtime identity.
        if !gents_store::identity::can_mint_defradb_bearer(&local.key.node_did) {
            return Err(BridgeError::new(BridgeErrorCode::Unsupported,
                "Open Runtime Settings to connect this runtime's configuration before choosing Approve for me."));
        }
        Some(
            core.prepare_access_review_task(
                &local.key.node_did,
                &local.key.agent_did,
                local.settings.as_ref().and_then(|settings| {
                    settings
                        .selection
                        .review_task
                        .as_ref()
                        .map(|task| task.task_id.as_str())
                }),
            )
            .await
            .map_err(super::backend_error)?,
        )
    } else {
        None
    };
    let home = local.home.clone();
    let key = local.key.clone();
    let settings = tokio::task::spawn_blocking(move || {
        workspace::save_mode(&home, &key, expected, request.mode, reviewer.as_ref())
    })
    .await
    .map_err(|error| super::backend_error(error.into()))?
    .map_err(super::backend_error)?;
    local.settings = Some(settings);
    Ok(local.view())
}

pub(super) async fn forget_commands(
    core: &ClientCore,
    request: WorkspacePermissionsForgetCommandsRequest,
) -> Result<WorkspacePermissionsState, BridgeError> {
    let expected: u64 = request.expected_revision.parse().map_err(|_| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Reload permissions before forgetting commands.",
        )
    })?;
    if expected.to_string() != request.expected_revision {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Reload permissions before forgetting commands.",
        ));
    }
    let Target::Local(mut local) = target(core, request.scope).await? else {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "This runtime cannot forget command permissions from the editor.",
        ));
    };
    if !local.support.remembered_commands {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "Update the runtime before managing remembered commands.",
        ));
    }
    if local.key.path != std::path::Path::new(&request.expected_workspace_path) {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The workspace changed. Reopen permissions.",
        ));
    }
    let home = local.home.clone();
    let key = local.key.clone();
    local.settings = Some(
        tokio::task::spawn_blocking(move || workspace::revoke_commands(&home, &key, expected))
            .await
            .map_err(|error| super::backend_error(error.into()))?
            .map_err(super::backend_error)?,
    );
    Ok(local.view())
}

async fn target(
    core: &ClientCore,
    request: WorkspacePermissionsRequest,
) -> Result<Target, BridgeError> {
    if request.agent_did.trim().is_empty() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Choose an agent and workspace before changing permissions.",
        ));
    }
    let record = super::approvals::peer(core, &request.node_did).await?;
    let support = gents_desktop_core::local_runtime::workspace_permission_support(&record)
        .await
        .map_err(super::backend_error)?;
    if record.operator_graphql().is_none() || record.local_node_home.is_none() {
        return Ok(Target::Unavailable(
            WorkspacePermissionsState::RemoteUnavailable { support },
        ));
    }
    let Some(support) = support else {
        return Ok(Target::Unavailable(
            WorkspacePermissionsState::UnsupportedRuntime,
        ));
    };
    let home = gents_desktop_core::local_runtime::hosted_runtime_home(&record)
        .map_err(super::backend_error)?
        .to_owned();
    let requester = core.node_identity().did().to_owned();
    tokio::task::spawn_blocking(move || local_target(&home, &requester, &request, support))
        .await
        .map_err(|error| super::backend_error(error.into()))?
        .map(|local| Target::Local(Box::new(local)))
        .map_err(super::backend_error)
}

fn local_target(
    home: &std::path::Path,
    requester: &str,
    request: &WorkspacePermissionsRequest,
    support: Support,
) -> anyhow::Result<LocalTarget> {
    let key = WorkspaceKey::canonical(
        &request.node_did,
        requester,
        &request.agent_did,
        std::path::Path::new(&request.workspace_path),
    )?;
    anyhow::ensure!(
        key.path.to_str().is_some(),
        "The workspace path must be valid UTF-8."
    );
    let settings = workspace::get(home, &key)?;
    Ok(LocalTarget {
        support,
        home: home.to_owned(),
        key,
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_permissions_does_not_enable_a_mode_or_create_host_settings() {
        let folder = tempfile::tempdir().unwrap();
        let home = folder.path().join("runtime");
        let mut request = WorkspacePermissionsRequest {
            node_did: "did:key:node".into(),
            agent_did: "engineer".into(),
            workspace_path: folder.path().to_str().unwrap().into(),
        };
        let support = Support {
            contract_version: workspace::CONTRACT_VERSION,
            workspace_sandbox: false,
            remembered_commands: true,
        };
        let read = |request: &WorkspacePermissionsRequest, requester: &str| {
            serde_json::to_value(
                local_target(&home, requester, request, support.clone())
                    .unwrap()
                    .view(),
            )
            .unwrap()
        };
        let unconfigured = read(&request, "did:key:client");
        assert!(unconfigured["mode"].is_null());
        assert_eq!(unconfigured["revision"], "0");
        assert!(!home.exists());
        let key = WorkspaceKey::canonical(
            &request.node_did,
            "did:key:client",
            &request.agent_did,
            folder.path(),
        )
        .unwrap();
        workspace::save_mode(&home, &key, 0, Mode::FullAccess, None).unwrap();
        let configured = read(&request, "did:key:client");
        assert_eq!(configured["mode"], "fullAccess");
        assert_eq!(configured["revision"], "1");
        assert!(read(&request, "did:key:other")["mode"].is_null());
        request.agent_did = "another".into();
        assert!(read(&request, "did:key:client")["mode"].is_null());
        std::fs::write(workspace::preferences_file(&home), b"invalid").unwrap();
        assert!(local_target(&home, "did:key:client", &request, support).is_err());
        assert_eq!(
            std::fs::read(workspace::preferences_file(&home)).unwrap(),
            b"invalid"
        );
    }
}
