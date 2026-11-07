use std::sync::Arc;

use anyhow::Result;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::Value;

use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;
use crate::tools::workspace::WorkspaceBindingDoc;

#[derive(Clone, Copy, Debug)]
enum RejectionOwner {
    FreshAdmission,
    ClaimAdmission,
}

pub(crate) struct Fixture {
    pub(crate) node: Arc<defra_node::Node>,
    pub(crate) request: crate::request::watcher::AgentRequest,
    pub(crate) binding: WorkspaceBindingDoc,
    pub(crate) access: ConfigAccess,
    agent: Arc<dyn NodeIdentity>,
    _workspace_dir: tempfile::TempDir,
    _home: crate::support::TestDb,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let db = crate::support::test_db("workspace-rejection").await;
        let (home, user, user_access, binding) =
            crate::test_support::signed_test_node_from_db(db, "workspace-rejection-user").await;
        let node = home.node.clone();
        let identity = home.node_identity.clone();
        let agent =
            crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let agent_did: identity::Did = agent.did().parse().unwrap();
        let admin = ConfigAccess::local_as(node.clone(), identity.did().parse().unwrap());
        admin
            .grant_node_permission(
                &agent_did,
                crate::store::config_client::NodePermission::DocumentDelete,
            )
            .await
            .unwrap();
        let access =
            ConfigAccess::local_managed_agent(node.clone(), agent_did, agent.as_ref()).unwrap();
        // Claim verification of a ReadWrite request loads the complete
        // workspace and its local placement, as the workspace owner publishes them.
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = crate::tools::workspace::IsolatedWorkspaceDoc {
            path_capability: crate::tools::workspace::WorkspacePathCapability::exact_paths(vec![])
                .unwrap(),
            workspace_id: "cleanup-workspace".into(),
            work_unit_id: "cleanup-work-unit".into(),
            repository_id: "cleanup-repository".into(),
            base_sha: "base".into(),
            branch: "cleanup-branch".into(),
            creation_policy: "git_worktree_diff".into(),
            adapter: "git_worktree".into(),
            owner_node_did: identity.did().into(),
            writer_principal: agent.did().into(),
            integrator_principal: agent.did().into(),
            instruction_manifest: "{}".into(),
            seal_hash: None,
            lifecycle_state: "ready".into(),
            caused_by_invocation_id: "cleanup-invocation".into(),
            caused_by_correlation: "cleanup-correlation".into(),
        };
        let placement = crate::tools::workspace::WorkspacePlacementDoc {
            workspace_id: workspace.workspace_id.clone(),
            owner_node_did: identity.did().into(),
            host_path: workspace_dir.path().to_string_lossy().into_owned(),
            repository_placement_id: "cleanup-repository-placement".into(),
            adapter: "git_worktree".into(),
            adapter_version: "1".into(),
            dirty_base: false,
            dirty_base_summary: String::new(),
            provisioning_state: "ready".into(),
            observed_tree_hash: String::new(),
        };
        let workspace_doc = access
            .transact_write("test.workspace_rejection_workspace", |txn| {
                let workspace = &workspace;
                let placement = &placement;
                Box::pin(async move {
                    let response = txn
                        .execute(&crate::tools::workspace::isolated_workspace_upsert_mutation(
                            workspace,
                        ))
                        .await?;
                    let doc = crate::store::graphql::mutation_documents(
                        &response,
                        "upsert_IsolatedWorkspace",
                    )?
                    .and_then(|rows| rows.first().and_then(|row| row["_docID"].as_str()).map(str::to_owned))
                    .expect("workspace physical ID");
                    txn.add_actor_relationship(
                        "IsolatedWorkspace",
                        &doc,
                        "writer",
                        &workspace.writer_principal.parse()?,
                    )
                    .await?;
                    let response = txn
                        .execute(&crate::tools::workspace::workspace_placement_upsert_mutation(
                            placement,
                            "2026-09-01T00:00:00Z",
                        ))
                        .await?;
                    let placement_doc = crate::store::graphql::mutation_documents(
                        &response,
                        "upsert_WorkspacePlacement",
                    )?
                    .and_then(|rows| rows.first().and_then(|row| row["_docID"].as_str()).map(str::to_owned))
                    .expect("placement physical ID");
                    txn.add_resource_relationship(
                        "WorkspacePlacement",
                        &placement_doc,
                        "parent",
                        "workspaces",
                        &doc,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(doc)
                })
            })
            .await
            .unwrap();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "workspace-rejected-request",
            identity.did(),
            user.did(),
            agent.did(),
            "session",
            "work",
            "interactive",
            "2026-09-01T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        create.workspace_id = Some("cleanup-workspace".to_string());
        create.workspace_owner_node_did = Some(identity.did().to_string());
        create.workspace_authority = Some("readWrite".to_string());
        let lifecycle =
            crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
                &node,
                &user_access,
                identity.clone(),
                &user,
                agent.as_ref(),
                create,
            )
            .await
            .expect("publish an authorized User request before workspace rejection");
        let request = lifecycle.request().clone();
        let doc_id = request.doc_id.clone();
        let binding = WorkspaceBindingDoc {
            workspace_doc_id: Some(workspace_doc),
            binding_id: "exact-binding".to_string(),
            workspace_id: "cleanup-workspace".to_owned(),
            request_id: request.request_id.clone(),
            request_doc_id: doc_id,
            authority: "readWrite".to_string(),
            owner_node_did: identity.did().to_string(),
            seal_hash: None,
            lifecycle_state: "active".to_string(),
        };
        let fixture = Self {
            node,
            request,
            binding,
            access,
            agent,
            _workspace_dir: workspace_dir,
            _home: home,
        };
        fixture.write_binding(&fixture.binding).await;
        fixture
    }

    pub(crate) async fn write_binding(&self, binding: &WorkspaceBindingDoc) {
        self.access
            .transact_write("test.workspace_rejection_binding", |txn| {
                Box::pin(async move {
                    let parent = binding
                        .workspace_doc_id
                        .as_deref()
                        .expect("fixture workspace binding");
                    let response = txn
                        .execute(&crate::tools::workspace::workspace_binding_upsert_mutation(
                            binding,
                        ))
                        .await?;
                    let rows = crate::store::graphql::mutation_documents(
                        &response,
                        "upsert_WorkspaceBinding",
                    )?
                    .expect("binding mutation returned a document");
                    assert_eq!(rows.len(), 1);
                    let doc = rows[0]["_docID"].as_str().expect("binding physical ID");
                    txn.add_resource_relationship(
                        "WorkspaceBinding",
                        doc,
                        "parent",
                        "workspaces",
                        parent,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    pub(crate) fn lifecycle(&self) -> crate::RequestLifecycle {
        let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            &self.request.agent_did,
            &self.request.node_did,
            self.request.clone(),
            60,
            crate::request::lifecycle::ExecutionOrigin::Interactive,
            "backend",
        );
        lifecycle.bind_managed_agent(self.agent.as_ref()).unwrap();
        lifecycle
    }

    async fn rejection_outcome(&self, doc_id: &str) -> Result<super::PendingRejectionOutcome> {
        Ok(super::terminalize_pending_request_rejection(
            &self.node,
            doc_id,
            &self.request.node_did,
            &self.request.agent_did,
            self.agent.as_ref(),
            "workspace placement is unavailable",
            "test.workspace_rejection_outcome",
        )
        .await?)
    }

    async fn reject(&self, owner: RejectionOwner) -> Result<()> {
        match owner {
            RejectionOwner::FreshAdmission => super::terminalize_pending_request_rejection(
                &self.node,
                &self.request.doc_id,
                &self.request.node_did,
                &self.request.agent_did,
                self.agent.as_ref(),
                "workspace placement is unavailable",
                "test.workspace_rejection",
            )
            .await
            .map(|_| ())
            .map_err(Into::into),
            RejectionOwner::ClaimAdmission => {
                self.lifecycle()
                    .reject_admission("workspace placement is unavailable")
                    .await
            }
        }
    }

    pub(crate) async fn observe(&self) -> Value {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        self.access.execute(
            &format!(r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ lifecycle_state failure_reason terminal_output }}
                WorkspaceBinding(order: {{ binding_id: ASC }}) {{ binding_id workspace_id request_id request_doc_id owner_node_did authority lifecycle_state }}
            }}"#),
        )
        .await
        .unwrap()
        ["data"].clone()
    }
}

#[tokio::test]
async fn malformed_pending_request_still_rejects_without_cleanup_provenance() {
    for field in ["request_id", "workspace_id", "workspace_owner_node_did"] {
        let mut fixture = Fixture::new().await;
        fixture.request.request_id = "malformed-workspace-request".to_string();
        let mut input = serde_json::json!({
            "request_id": fixture.request.request_id,
            "node_did": fixture.request.node_did,
            "agent_did": fixture.request.agent_did,
            "purpose": "normal",
            "lifecycle_state": "pending",
            "workspace_id": fixture.binding.workspace_id,
            "workspace_owner_node_did": fixture.binding.owner_node_did,
        });
        input[field] = Value::Null;
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        let created = fixture
            .access
            .write(
                "test.malformed_workspace_rejection",
                &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
        fixture.request.doc_id =
            crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
        fixture.binding.binding_id = "malformed-binding".to_string();
        fixture.binding.request_id = fixture.request.request_id.clone();
        fixture.binding.request_doc_id = fixture.request.doc_id.clone();
        fixture.write_binding(&fixture.binding).await;
        fixture
            .reject(RejectionOwner::FreshAdmission)
            .await
            .unwrap();
        let observed = fixture.observe().await;
        assert_eq!(
            observed["AgentRequest"][0]["lifecycle_state"], "failed",
            "missing {field}"
        );
        assert!(
            observed["WorkspaceBinding"]
                .as_array()
                .unwrap()
                .iter()
                .all(|binding| binding["lifecycle_state"] == "active"),
            "missing {field}"
        );
    }
}

#[tokio::test]
async fn pending_rejection_releases_only_its_exact_binding_and_only_once() {
    for owner in [
        RejectionOwner::FreshAdmission,
        RejectionOwner::ClaimAdmission,
    ] {
        let fixture = Fixture::new().await;
        for field in ["workspace", "node", "logical_request", "physical_request"] {
            let mut foreign = fixture.binding.clone();
            foreign.binding_id = format!("foreign-{field}");
            match field {
                "workspace" => foreign.workspace_id.push_str("-other"),
                "node" => foreign.owner_node_did.push_str("-other"),
                "logical_request" => foreign.request_id.push_str("-other"),
                "physical_request" => foreign.request_doc_id.push_str("-other"),
                _ => unreachable!(),
            }
            fixture.write_binding(&foreign).await;
        }
        fixture.reject(owner).await.unwrap();
        let observed = fixture.observe().await;
        assert_eq!(
            observed["AgentRequest"][0]["lifecycle_state"], "failed",
            "{owner:?}"
        );
        for binding in observed["WorkspaceBinding"].as_array().unwrap() {
            assert_eq!(
                binding["lifecycle_state"],
                if binding["binding_id"] == "exact-binding" {
                    "released"
                } else {
                    "active"
                },
                "{owner:?}: {binding}",
            );
        }

        fixture.write_binding(&fixture.binding).await;
        let before_replay = fixture.observe().await;
        fixture.reject(owner).await.unwrap();
        assert_eq!(fixture.observe().await, before_replay, "{owner:?}");
    }
}

#[tokio::test]
async fn pending_rejection_and_binding_release_roll_back_together() {
    for owner in [
        RejectionOwner::FreshAdmission,
        RejectionOwner::ClaimAdmission,
    ] {
        let fixture = Fixture::new().await;
        let (_, mutations) = ConfigApplyTxn::assert_every_mutation_rolls_back(
            || fixture.reject(owner),
            || fixture.observe(),
        )
        .await;
        assert!(
            mutations >= 2,
            "{owner:?}: terminal and binding must share the transaction"
        );
        let observed = fixture.observe().await;
        assert_eq!(observed["AgentRequest"][0]["lifecycle_state"], "failed");
        assert_eq!(
            observed["WorkspaceBinding"][0]["lifecycle_state"],
            "released"
        );
    }
}

#[tokio::test]
async fn pending_rejection_cannot_release_a_request_that_was_claimed() {
    for owner in [
        RejectionOwner::FreshAdmission,
        RejectionOwner::ClaimAdmission,
    ] {
        let fixture = Fixture::new().await;
        let doc_id = escape_graphql_string(&fixture.request.doc_id);
        fixture
            .access
            .write(
                "test.workspace_rejection_lost_cas",
                &format!(
                    r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ lifecycle_state: "claimed" }}
            ) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();
        let before = fixture.observe().await;
        if matches!(owner, RejectionOwner::FreshAdmission) {
            assert_eq!(
                fixture
                    .rejection_outcome(&fixture.request.doc_id)
                    .await
                    .unwrap(),
                super::PendingRejectionOutcome::NotMatched
            );
        }
        let result = fixture.reject(owner).await;
        assert_eq!(
            result.is_ok(),
            matches!(owner, RejectionOwner::FreshAdmission)
        );
        assert_eq!(fixture.observe().await, before, "{owner:?}");
    }
}

#[tokio::test]
async fn pending_rejection_reports_only_an_applied_update() {
    use super::PendingRejectionOutcome;

    let fixture = Fixture::new().await;
    let created = fixture.access.write(
        "test.workspace_rejection_absent_create",
        r#"mutation { create_AgentRequest(input: { request_id: "deleted-request" }) { _docID } }"#,
    )
    .await
    .unwrap();
    let absent = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    fixture
        .access
        .write(
            "test.workspace_rejection_absent_delete",
            &format!(
                r#"mutation {{ delete_AgentRequest(docID: "{}") {{ _docID }} }}"#,
                escape_graphql_string(&absent)
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.rejection_outcome(&absent).await.unwrap(),
        PendingRejectionOutcome::NotMatched
    );
    assert_eq!(
        fixture
            .rejection_outcome(&fixture.request.doc_id)
            .await
            .unwrap(),
        PendingRejectionOutcome::Terminalized
    );
    let before_replay = fixture.observe().await;
    assert_eq!(
        fixture
            .rejection_outcome(&fixture.request.doc_id)
            .await
            .unwrap(),
        PendingRejectionOutcome::NotMatched
    );
    assert_eq!(fixture.observe().await, before_replay);
}

#[tokio::test]
async fn pending_rejection_reports_write_failure_and_rolls_back() {
    let fixture = Fixture::new().await;
    let before = fixture.observe().await;
    let (result, mutations) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        super::terminalize_pending_request_rejection(
            &fixture.node,
            &fixture.request.doc_id,
            &fixture.request.node_did,
            &fixture.request.agent_did,
            fixture.agent.as_ref(),
            "workspace placement is unavailable",
            "test.workspace_rejection_write_error",
        ),
    )
    .await;
    assert_eq!(mutations, 1);
    assert!(matches!(
        result,
        Err(super::PendingRejectionError::Transaction(_))
    ));
    assert_eq!(fixture.observe().await, before);
}
