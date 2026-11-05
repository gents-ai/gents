use super::*;
use crate::store::config_client::{ConfigAccess, NodePermission};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::output::{OutputSource, OutputWriter};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
struct Case {
    name: String,
    parent_granted: bool,
    parent_linked: bool,
}

async fn create_document(access: &ConfigAccess, collection: &str, input: Value) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let result = access
        .write(
            "test.auxiliary_child.fixture",
            &format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    crate::store::graphql::created_doc_id(&result, collection).unwrap()
}

async fn visible(access: &ConfigAccess, doc: &str) -> bool {
    let response = access
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(doc)
        ))
        .await
        .unwrap();
    !response["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .is_empty()
}

#[tokio::test]
async fn modeled_auxiliary_child_repair_requires_signed_exact_parent_and_native_owner() {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().auxiliary_session_child_binding_cases;
    let selected = [
        "paired_title_segment",
        "missing_parent",
        "invalid_parent_signature",
        "foreign_native_owner",
        "completed_then_revoked",
    ];
    let mut exercised = std::collections::BTreeSet::new();
    for value in cases {
        let case: Case = serde_json::from_value(value.clone()).unwrap();
        if !selected.contains(&case.name.as_str()) {
            continue;
        }
        exercised.insert(case.name.clone());
        let temp = tempfile::tempdir().unwrap();
        let root = KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap();
        let reader = KeyIdentity::load_or_create(temp.path().join("reader.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(root.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let root_access = ConfigAccess::local_as(node.clone(), root.did().parse().unwrap());
        let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let foreign_binding = crate::store::identity::provision_agent_identity(&root).unwrap();
        let foreign =
            crate::store::identity::resolve_agent_identity(&root, &foreign_binding).unwrap();
        for actor in [agent.did(), foreign.did()] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
                NodePermission::DacRelationDelete,
            ] {
                root_access
                    .grant_node_permission(&actor.parse().unwrap(), permission)
                    .await
                    .unwrap();
            }
        }
        root_access
            .grant_node_permission(&reader.did().parse().unwrap(), NodePermission::DocumentRead)
            .await
            .unwrap();
        let missing_parent_doc =
            create_document(&root_access, "Node", json!({"node_did": root.did()})).await;
        let agent_doc = create_document(
            &root_access,
            "Agent",
            json!({"node_did":root.did(), "agent_did":agent.did(), "creation_nonce":binding.creation_nonce}),
        )
        .await;
        let session_id = format!("repair-{}", uuid::Uuid::new_v4());
        let session_doc = create_document(
            &root_access,
            "AgentSession",
            json!({"session_id":session_id, "node_did":root.did(), "agent_did":agent.did(),
                "requester_did":root.did(), "created_at":"2026-10-21T00:00:00Z"}),
        )
        .await;
        for (relation, actor) in [("working_agent", agent.did()), ("reader", reader.did())] {
            root_access
                .add_actor_relationship(
                    "AgentSession",
                    &session_doc,
                    relation,
                    &actor.parse().unwrap(),
                )
                .await
                .unwrap();
        }
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let foreign_access = ConfigAccess::local_managed_agent(
            node.clone(),
            foreign.did().parse().unwrap(),
            foreign.as_ref(),
        )
        .unwrap();
        let reader_access = ConfigAccess::local_as(node.clone(), reader.did().parse().unwrap());
        let parent_id = format!("parent-{}", uuid::Uuid::new_v4());
        let mut parent = AgentRequestCreate::base(
            RequestPurpose::Normal,
            &parent_id,
            root.did(),
            root.did(),
            agent.did(),
            &session_id,
            "parent",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::client(root.did()),
        );
        parent.session_doc_id = Some(session_doc.clone());
        parent.agent_doc_id = Some(agent_doc);
        crate::sign_agent_request_create(&root, &mut parent)
            .await
            .unwrap();
        if case.name == "invalid_parent_signature" {
            parent.content.push_str(" tampered");
        }
        let parent_doc = if case.name == "missing_parent" {
            missing_parent_doc
        } else {
            let created = root_access
                .write(
                    "test.auxiliary_child.parent",
                    &parent.graphql_mutation().unwrap(),
                )
                .await
                .unwrap();
            let doc = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
            root_access
                .add_actor_relationship(
                    "AgentRequest",
                    &doc,
                    "working_agent",
                    &agent.did().parse().unwrap(),
                )
                .await
                .unwrap();
            doc
        };
        let aux_id = format!("aux-{}", uuid::Uuid::new_v4());
        let mut aux = AgentRequestCreate::base(
            RequestPurpose::TitleAudit,
            &aux_id,
            root.did(),
            agent.did(),
            agent.did(),
            &session_id,
            "audit",
            "interactive",
            "2026-10-21T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_local_control(agent.did(), &parent_id),
        );
        aux.max_retries = 0;
        aux.caused_by_parent_request_id = Some(parent_id);
        aux.caused_by_parent_request_doc_id = Some(parent_doc);
        crate::sign_agent_request_create(agent.as_ref(), &mut aux)
            .await
            .unwrap();
        let result = agent_access
            .write(
                "test.auxiliary_child.request",
                &aux.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let aux_doc = crate::store::graphql::created_doc_id(&result, "AgentRequest").unwrap();
        let source = OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Title,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        };
        let input = json!({
            "node_did":root.did(), "requester_did":agent.did(), "session_id":session_id,
            "session_doc_id":session_doc, "agent_did":agent.did(), "creator_did":agent.did(),
            "parent_linked":false, "request_doc_id":aux_doc, "source":source,
            "ordinal":0, "writer":OutputWriter::RequestExecution { execution_generation:"fixture".into() },
            "payload":"audit", "created_at":"2026-10-21T00:00:00Z",
        });
        let creator = if case.name == "foreign_native_owner" {
            &foreign_access
        } else {
            &agent_access
        };
        let result = creator
            .transact_write("test.auxiliary_child.create", |txn| {
                let variables = json!({"input": input});
                Box::pin(async move {
                    txn.execute_with_variables(
                        crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &variables,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let child_doc =
            crate::store::graphql::created_doc_id(&result, "AgentOutputSegment").unwrap();
        if case.name == "foreign_native_owner" {
            foreign_access
                .add_actor_relationship(
                    "AgentOutputSegment",
                    &child_doc,
                    "reader",
                    &agent.did().parse().unwrap(),
                )
                .await
                .unwrap();
        }
        let child_ids = [child_doc.clone()];
        let owners = agent_access
            .transact_readonly("test.auxiliary_child.native_owner", |txn| {
                let child_ids = &child_ids;
                Box::pin(async move { txn.document_owners("AgentOutputSegment", child_ids).await })
            })
            .await
            .unwrap();
        let expected_owner = if case.name == "foreign_native_owner" {
            foreign.did()
        } else {
            agent.did()
        };
        assert_eq!(owners.len(), 1, "{}", case.name);
        assert_eq!(
            owners[0].owner.as_ref().map(|did| did.as_str()),
            Some(expected_owner),
            "{}",
            case.name
        );
        if case.name == "completed_then_revoked" {
            agent_access
                .add_resource_relationship(
                    "AgentOutputSegment",
                    &child_doc,
                    "parent",
                    "sessions",
                    &session_doc,
                )
                .await
                .unwrap();
            agent_access.write("test.auxiliary_child.mark", &format!(r#"mutation {{ update_AgentOutputSegment(docID: "{}", input: {{ parent_linked: true }}) {{ _docID }} }}"#, escape_graphql_string(&child_doc))).await.unwrap();
            agent_access
                .delete_resource_relationship(
                    "AgentOutputSegment",
                    &child_doc,
                    "parent",
                    "sessions",
                    &session_doc,
                )
                .await
                .unwrap();
        }
        assert!(!visible(&reader_access, &child_doc).await, "{}", case.name);
        recover_agent_session_child_relationships_with_access(
            &agent_access,
            root.did(),
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            visible(&reader_access, &child_doc).await,
            case.parent_granted,
            "{}",
            case.name
        );
        let state = creator.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ parent_linked }} }}"#, escape_graphql_string(&child_doc))).await.unwrap();
        assert_eq!(
            state["data"]["AgentOutputSegment"][0]["parent_linked"].as_bool(),
            Some(case.parent_linked),
            "{}",
            case.name
        );
        node.shutdown().await.unwrap();
    }
    assert_eq!(
        exercised.len(),
        selected.len(),
        "each selected Lean case must be exercised"
    );
}
