use std::sync::Arc;

use anyhow::Result;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::Value;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::identity::{KeyIdentity, NodeIdentity};
use crate::workspace::WorkspaceBindingDoc;

#[derive(Clone, Copy, Debug)]
enum RejectionOwner {
    FreshAdmission,
    ClaimAdmission,
}

pub(crate) struct Fixture {
    pub(crate) node: Arc<defra_node::EmbeddedNode>,
    pub(crate) request: crate::watcher::AgentRequest,
    pub(crate) binding: WorkspaceBindingDoc,
    _home: tempfile::TempDir,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(home.path().join("identity.key"), None).unwrap();
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "workspace-rejected-request",
            identity.did(),
            identity.did(),
            "agent",
            "session",
            "work",
            "interactive",
            "2026-09-01T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.workspace_id = Some("cleanup-workspace".to_string());
        create.workspace_owner_node_did = Some(identity.did().to_string());
        create.workspace_authority = Some("readWrite".to_string());
        crate::sign_agent_request_create(&identity, &mut create)
            .await
            .unwrap();
        let response = ConfigAccess::write_local(
            &node,
            "test.workspace_rejection_request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
        let doc_id = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        let request = super::load_request_for_admission_test(&node, &doc_id)
            .await
            .unwrap();
        let binding = WorkspaceBindingDoc {
            binding_id: "exact-binding".to_string(),
            workspace_id: create.workspace_id.unwrap(),
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
            _home: home,
        };
        fixture.write_binding(&fixture.binding).await;
        fixture
    }

    pub(crate) async fn write_binding(&self, binding: &WorkspaceBindingDoc) {
        ConfigAccess::write_local(
            &self.node,
            "test.workspace_rejection_binding",
            &crate::workspace::workspace_binding_upsert_mutation(binding),
        )
        .await
        .unwrap();
    }

    pub(crate) fn lifecycle(&self) -> crate::RequestLifecycle {
        crate::RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            "agent",
            &self.request.node_did,
            self.request.clone(),
            60,
            crate::lifecycle::ExecutionOrigin::Interactive,
            "backend",
        )
    }

    async fn reject(&self, owner: RejectionOwner) -> Result<()> {
        match owner {
            RejectionOwner::FreshAdmission => {
                super::terminalize_pending_request_rejection(
                    &self.node,
                    &self.request.doc_id,
                    &self.request.node_did,
                    "workspace placement is unavailable",
                    "test.workspace_rejection",
                )
                .await
            }
            RejectionOwner::ClaimAdmission => {
                self.lifecycle()
                    .reject_admission("workspace placement is unavailable")
                    .await
            }
        }
    }

    pub(crate) async fn observe(&self) -> Value {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        graphql_with_transaction_retry(
            &self.node,
            &format!(r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ lifecycle_state failure_reason terminal_output }}
                WorkspaceBinding(order: {{ binding_id: ASC }}) {{ binding_id workspace_id request_id request_doc_id owner_node_did authority lifecycle_state }}
            }}"#),
            "test.workspace_rejection_observation",
        )
        .await
        .unwrap()
        .data
        .unwrap()
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
            "purpose": "normal",
            "lifecycle_state": "pending",
            "workspace_id": fixture.binding.workspace_id,
            "workspace_owner_node_did": fixture.binding.owner_node_did,
        });
        input[field] = Value::Null;
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        let created = ConfigAccess::write_local(
            &fixture.node,
            "test.malformed_workspace_rejection",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
        fixture.request.doc_id = crate::graphql::created_doc_id(&created, "AgentRequest").unwrap();
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

        // A replay that did not win the terminal CAS has no cleanup authority.
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
        ConfigAccess::write_local(
            &fixture.node,
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
        let result = fixture.reject(owner).await;
        assert_eq!(
            result.is_ok(),
            matches!(owner, RejectionOwner::FreshAdmission)
        );
        assert_eq!(fixture.observe().await, before, "{owner:?}");
    }
}
