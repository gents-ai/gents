use anyhow::Result;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_session::workspace::WorkspaceBindingDoc;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub enum RejectionOwner {
    FreshAdmission,
    ClaimAdmission,
}

pub struct Fixture {
    pub node: Arc<defra_node::Node>,
    pub request: gents_protocol::row::AgentRequest,
    pub binding: WorkspaceBindingDoc,
    pub access: ConfigAccess,
    pub agent: Arc<dyn NodeIdentity>,
    pub _workspace_dir: tempfile::TempDir,
    pub _home: gents_inference::test_support::TestDb,
}

impl Fixture {
    pub async fn new() -> Self {
        let db = gents_inference::test_support::test_db(
            "workspace-rejection",
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
        let (home, user, user_access, binding) =
            gents_inference::test_support::signed_test_node_from_db(db, "workspace-rejection-user")
                .await;
        let node = home.node.clone();
        let identity = home.node_identity.clone();
        let agent =
            gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let agent_did: identity::Did = agent.did().parse().unwrap();
        let admin = ConfigAccess::local_as(node.clone(), identity.did().parse().unwrap());
        admin
            .grant_node_permission(
                &agent_did,
                gents_store::config_client::NodePermission::DocumentDelete,
            )
            .await
            .unwrap();
        let access =
            ConfigAccess::local_managed_agent(node.clone(), agent_did, agent.as_ref()).unwrap();
        // Claim verification of a ReadWrite request loads the complete
        // workspace and its local placement, as the workspace owner publishes them.
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = gents_session::workspace::IsolatedWorkspaceDoc {
            path_capability: gents_session::workspace::WorkspacePathCapability::exact_paths(vec![])
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
            route: Default::default(),
        };
        let placement = gents_session::workspace::WorkspacePlacementDoc {
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
                        .execute(
                            &gents_session::workspace::isolated_workspace_upsert_mutation(
                                workspace,
                            ),
                        )
                        .await?;
                    let doc = gents_store::graphql::mutation_documents(
                        &response,
                        "upsert_IsolatedWorkspace",
                    )?
                    .and_then(|rows| {
                        rows.first()
                            .and_then(|row| row["_docID"].as_str())
                            .map(str::to_owned)
                    })
                    .expect("workspace physical ID");
                    txn.add_actor_relationship(
                        "IsolatedWorkspace",
                        &doc,
                        "writer",
                        &workspace.writer_principal.parse()?,
                    )
                    .await?;
                    let response = txn
                        .execute(
                            &gents_session::workspace::workspace_placement_upsert_mutation(
                                placement,
                                "2026-09-01T00:00:00Z",
                            ),
                        )
                        .await?;
                    let placement_doc = gents_store::graphql::mutation_documents(
                        &response,
                        "upsert_WorkspacePlacement",
                    )?
                    .and_then(|rows| {
                        rows.first()
                            .and_then(|row| row["_docID"].as_str())
                            .map(str::to_owned)
                    })
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
        let lifecycle = crate::call::lifecycle::admission_fixture::pending_client_fixture_request(
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

    pub async fn write_binding(&self, binding: &WorkspaceBindingDoc) {
        self.access
            .transact_write("test.workspace_rejection_binding", |txn| {
                Box::pin(async move {
                    let parent = binding
                        .workspace_doc_id
                        .as_deref()
                        .expect("fixture workspace binding");
                    let response = txn
                        .execute(
                            &gents_session::workspace::workspace_binding_upsert_mutation(binding),
                        )
                        .await?;
                    let rows = gents_store::graphql::mutation_documents(
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

    pub fn lifecycle(&self) -> gents_session::request::lifecycle::RequestLifecycle {
        let mut lifecycle =
            gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                self.node.clone(),
                self.request.clone(),
                60,
                gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                "backend",
            );
        lifecycle.bind_managed_agent(self.agent.as_ref()).unwrap();
        lifecycle
    }

    pub async fn rejection_outcome(
        &self,
        doc_id: &str,
    ) -> Result<gents_session::request::admission::PendingRejectionOutcome> {
        Ok(
            gents_session::request::admission::terminalize_pending_request_rejection(
                &self.node,
                doc_id,
                &self.request.node_did,
                &self.request.agent_did,
                self.agent.as_ref(),
                "workspace placement is unavailable",
                "test.workspace_rejection_outcome",
            )
            .await?,
        )
    }

    pub async fn reject(&self, owner: RejectionOwner) -> Result<()> {
        match owner {
            RejectionOwner::FreshAdmission => {
                gents_session::request::admission::terminalize_pending_request_rejection(
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
                .map_err(Into::into)
            }
            RejectionOwner::ClaimAdmission => {
                self.lifecycle()
                    .reject_admission("workspace placement is unavailable")
                    .await
            }
        }
    }

    pub async fn observe(&self) -> Value {
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
