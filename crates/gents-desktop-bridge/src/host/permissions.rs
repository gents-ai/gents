use gents::plugin::approval::workspace::{self, Mode, Support, WorkspaceKey};
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
    /// The chat the choice applies to. Omitted addresses the agent and
    /// workspace default, which governs every chat that has not chosen.
    #[serde(default)]
    #[ts(optional)]
    pub session_id: Option<String>,
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

/// Which stored selection the reported state and the next save address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkspacePermissionScope {
    Default,
    Session { session_id: String },
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
        scope: WorkspacePermissionScope,
        /// The reported selection is the agent and workspace default because
        /// this chat has not chosen one. Saving stores the chat's own entry.
        inherited: bool,
    },
}

struct LocalTarget {
    support: Support,
    home: std::path::PathBuf,
    key: WorkspaceKey,
    session_id: Option<String>,
    settings: Option<workspace::Settings>,
}

impl LocalTarget {
    fn inherited(&self) -> bool {
        self.session_id.is_some()
            && self
                .settings
                .as_ref()
                .is_some_and(|settings| settings.workspace.session_id.is_none())
    }

    fn view(self) -> WorkspacePermissionsState {
        let scope = match &self.session_id {
            Some(session_id) => WorkspacePermissionScope::Session {
                session_id: session_id.clone(),
            },
            None => WorkspacePermissionScope::Default,
        };
        let inherited = self.inherited();
        WorkspacePermissionsState::Local {
            scope,
            inherited,
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
    let (key, stored_revision) = save_target(&local, expected)?;
    let reviewer = if request.mode == Mode::AutoReview {
        // Configuring the Task may use only the existing signer cache; opening
        // the permission picker must not unlock a runtime identity.
        if !gents::store::identity::can_mint_defradb_bearer(&local.key.node_did) {
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
    let settings = tokio::task::spawn_blocking(move || {
        workspace::save_mode(
            &home,
            &key,
            stored_revision,
            request.mode,
            reviewer.as_ref(),
        )
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
    let key = forget_target(&local, expected)?;
    let home = local.home.clone();
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
    if let Some(session_id) = session_label(&request) {
        // The label must name a session this client is the requester of. ACP
        // decides what the client holds; nothing here is a second identity.
        let store = core.store().snapshot();
        let authorized = store.sessions.iter().any(|row| {
            row.session_id == session_id
                && row.node_did == request.node_did
                && row.agent_did.trim() == request.agent_did.trim()
                && row.requester_did.as_deref() == Some(requester.as_str())
        });
        if !authorized {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "Start the chat before choosing its permissions.",
            ));
        }
    }
    tokio::task::spawn_blocking(move || local_target(&home, &requester, &request, support))
        .await
        .map_err(|error| super::backend_error(error.into()))?
        .map(|local| Target::Local(Box::new(local)))
        .map_err(super::backend_error)
}

/// The entry a save addresses and the revision it must still hold. The stale
/// guard compares the selection the editor actually saw; for a chat that has
/// not chosen that is the agent and folder default, while the chat's own entry
/// starts from nothing.
fn save_target(local: &LocalTarget, expected: u64) -> Result<(WorkspaceKey, u64), BridgeError> {
    if observed_revision(local) != expected {
        return Err(super::backend_error(
            workspace::SelectionError::Stale.into(),
        ));
    }
    let key = local
        .key
        .with_session(local.session_id.as_deref())
        .map_err(super::backend_error)?;
    Ok((key, if local.inherited() { 0 } else { expected }))
}

/// Remembered commands are forgotten where they are stored, which is the entry
/// that governs this chat.
fn forget_target(local: &LocalTarget, expected: u64) -> Result<WorkspaceKey, BridgeError> {
    if observed_revision(local) != expected {
        return Err(super::backend_error(
            workspace::SelectionError::Stale.into(),
        ));
    }
    Ok(local
        .settings
        .as_ref()
        .map_or_else(|| local.key.clone(), |settings| settings.workspace.clone()))
}

fn observed_revision(local: &LocalTarget) -> u64 {
    local
        .settings
        .as_ref()
        .map_or(0, |settings| settings.selection.revision)
}

fn session_label(request: &WorkspacePermissionsRequest) -> Option<&str> {
    request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
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
    let session_id = session_label(request);
    let settings = workspace::resolve(home, &key, session_id)?;
    Ok(LocalTarget {
        support,
        home: home.to_owned(),
        key,
        session_id: session_id.map(ToOwned::to_owned),
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support() -> Support {
        Support {
            contract_version: workspace::CONTRACT_VERSION,
            workspace_sandbox: false,
            remembered_commands: true,
            session_scoped: true,
        }
    }

    #[test]
    fn reading_permissions_does_not_enable_a_mode_or_create_host_settings() {
        let folder = tempfile::tempdir().unwrap();
        let home = folder.path().join("runtime");
        let mut request = WorkspacePermissionsRequest {
            node_did: "did:key:node".into(),
            agent_did: "engineer".into(),
            workspace_path: folder.path().to_str().unwrap().into(),
            session_id: None,
        };
        let read = |request: &WorkspacePermissionsRequest, requester: &str| {
            serde_json::to_value(
                local_target(&home, requester, request, support())
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
        assert_eq!(configured["scope"], serde_json::json!({"kind": "default"}));
        assert_eq!(configured["inherited"], false);
        assert!(read(&request, "did:key:other")["mode"].is_null());
        request.agent_did = "another".into();
        assert!(read(&request, "did:key:client")["mode"].is_null());
        std::fs::write(workspace::preferences_file(&home), b"invalid").unwrap();
        assert!(local_target(&home, "did:key:client", &request, support()).is_err());
        assert_eq!(
            std::fs::read(workspace::preferences_file(&home)).unwrap(),
            b"invalid"
        );
    }

    #[test]
    fn a_chat_reports_the_folder_default_until_it_chooses_its_own() {
        let folder = tempfile::tempdir().unwrap();
        let home = folder.path().join("runtime");
        let request = |session: Option<&str>| WorkspacePermissionsRequest {
            node_did: "did:key:node".into(),
            agent_did: "engineer".into(),
            workspace_path: folder.path().to_str().unwrap().into(),
            session_id: session.map(ToOwned::to_owned),
        };
        let target = |session: Option<&str>| {
            local_target(&home, "did:key:client", &request(session), support()).unwrap()
        };
        let read = |session: Option<&str>| serde_json::to_value(target(session).view()).unwrap();
        let fresh = read(Some("chat"));
        assert!(fresh["mode"].is_null());
        assert_eq!(fresh["inherited"], false);
        assert_eq!(
            fresh["scope"],
            serde_json::json!({"kind": "session", "sessionId": "chat"})
        );
        assert!(!home.exists());
        let key =
            WorkspaceKey::canonical("did:key:node", "did:key:client", "engineer", folder.path())
                .unwrap();
        let default = workspace::save_mode(&home, &key, 0, Mode::Ask, None).unwrap();
        let inherited = read(Some("chat"));
        assert_eq!(inherited["mode"], "ask");
        assert_eq!(inherited["revision"], "1");
        assert_eq!(inherited["inherited"], true);
        // the editor saw the default, and this chat's own entry starts fresh
        let (written, revision) = save_target(&target(Some("chat")), 1).unwrap();
        assert_eq!(written.session_id.as_deref(), Some("chat"));
        assert_eq!(revision, 0);
        assert!(save_target(&target(Some("chat")), 0).is_err());
        workspace::save_mode(&home, &written, revision, Mode::FullAccess, None).unwrap();
        let chosen = read(Some("chat"));
        assert_eq!(chosen["mode"], "fullAccess");
        assert_eq!(chosen["inherited"], false);
        assert_eq!(chosen["revision"], "1");
        assert_eq!(save_target(&target(Some("chat")), 1).unwrap(), (written, 1));
        assert_eq!(read(Some("other"))["mode"], "ask");
        let unchanged = read(None);
        assert_eq!(unchanged["mode"], "ask");
        assert_eq!(unchanged["scope"], serde_json::json!({"kind": "default"}));
        assert_eq!(
            unchanged["revision"],
            default.selection.revision.to_string()
        );
        // forgetting commands acts on the entry that governs the chat
        assert_eq!(
            forget_target(&target(Some("other")), 1).unwrap().session_id,
            None
        );
        assert_eq!(
            forget_target(&target(Some("chat")), 1)
                .unwrap()
                .session_id
                .as_deref(),
            Some("chat")
        );
    }
}
