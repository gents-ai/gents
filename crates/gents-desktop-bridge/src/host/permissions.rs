use gents::plugin::approval::workspace::{self, Mode, Support, WorkspaceKey};
use gents_desktop_core::client::ClientCore;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspacePermissionsRequest {
    pub node_did: String,
    pub agent_id: String,
    pub workspace_path: String,
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
        /// Revisions cross JavaScript as decimal strings without rounding.
        revision: String,
    },
}

pub(super) async fn state(
    core: &ClientCore,
    request: WorkspacePermissionsRequest,
) -> Result<WorkspacePermissionsState, BridgeError> {
    if request.agent_id.trim().is_empty() || request.workspace_path.trim().is_empty() {
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
        return Ok(WorkspacePermissionsState::RemoteUnavailable { support });
    }
    let Some(support) = support else {
        return Ok(WorkspacePermissionsState::UnsupportedRuntime);
    };
    let home = gents_desktop_core::local_runtime::hosted_runtime_home(&record)
        .map_err(super::backend_error)?
        .to_owned();
    let requester = core.node_identity().did().to_owned();
    tokio::task::spawn_blocking(move || local_state(&home, &requester, &request, support))
        .await
        .map_err(|error| super::backend_error(error.into()))?
        .map_err(super::backend_error)
}

fn local_state(
    home: &std::path::Path,
    requester: &str,
    request: &WorkspacePermissionsRequest,
    support: Support,
) -> anyhow::Result<WorkspacePermissionsState> {
    let key = WorkspaceKey::canonical(
        &request.node_did,
        requester,
        &request.agent_id,
        std::path::Path::new(&request.workspace_path),
    )?;
    let settings = workspace::get(home, &key)?;
    Ok(WorkspacePermissionsState::Local {
        support,
        mode: settings.as_ref().map(|settings| settings.selection.mode),
        revision: settings
            .map_or(0, |settings| settings.selection.revision)
            .to_string(),
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
            agent_id: "engineer".into(),
            workspace_path: folder.path().to_str().unwrap().into(),
        };
        let support = Support {
            contract_version: workspace::CONTRACT_VERSION,
            workspace_sandbox: false,
        };
        let read = |request: &WorkspacePermissionsRequest, requester: &str| {
            serde_json::to_value(local_state(&home, requester, request, support.clone()).unwrap())
                .unwrap()
        };
        let unconfigured = read(&request, "did:key:client");
        assert!(unconfigured["mode"].is_null());
        assert_eq!(unconfigured["revision"], "0");
        assert!(!home.exists());
        let key = WorkspaceKey::canonical(
            &request.node_did,
            "did:key:client",
            &request.agent_id,
            folder.path(),
        )
        .unwrap();
        workspace::save_mode(&home, &key, 0, Mode::FullAccess, None).unwrap();
        let configured = read(&request, "did:key:client");
        assert_eq!(configured["mode"], "fullAccess");
        assert_eq!(configured["revision"], "1");
        assert!(read(&request, "did:key:other")["mode"].is_null());
        request.agent_id = "another".into();
        assert!(read(&request, "did:key:client")["mode"].is_null());
        std::fs::write(workspace::preferences_file(&home), b"invalid").unwrap();
        assert!(local_state(&home, "did:key:client", &request, support).is_err());
        assert_eq!(
            std::fs::read(workspace::preferences_file(&home)).unwrap(),
            b"invalid"
        );
    }
}
