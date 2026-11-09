use super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::output::OutputSource;
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
    binding: Value,
    allowed: bool,
}

fn text<'a>(binding: &'a Value, field: &str) -> &'a str {
    binding[field].as_str().expect("modeled binding text")
}

async fn create_document(access: &crate::ConfigAccess, collection: &str, input: Value) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let result = access
        .write(
            "test.session_child.fixture",
            &format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    crate::store::graphql::created_doc_id(&result, collection).unwrap()
}

#[tokio::test]
async fn modeled_request_child_bindings_use_real_node_and_agent_signatures() {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().auxiliary_session_child_binding_cases;
    let selected = [
        "paired_title_segment",
        "paired_title_capture",
        "paired_review_segment",
        "paired_review_capture",
        "ordinary_same_requester",
        "ordinary_foreign_requester",
        "ordinary_absent_requester",
        "source_kind_mismatch",
        "foreign_actor",
        "foreign_child_node",
        "foreign_child_session",
        "foreign_child_requester",
        "missing_auxiliary",
        "foreign_auxiliary_document",
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
        let node = Arc::new(
            crate::defra_node::Node::builder()
                .with_node_identity_did(root.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let root_access = crate::ConfigAccess::local_as(node.clone(), root.did().parse().unwrap());
        create_document(&root_access, "Node", json!({"node_did": root.did()})).await;
        let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let foreign_binding = crate::store::identity::provision_agent_identity(&root).unwrap();
        let foreign =
            crate::store::identity::resolve_agent_identity(&root, &foreign_binding).unwrap();
        let agent_doc = create_document(&root_access, "Agent", json!({
            "node_did":root.did(), "agent_did":agent.did(), "creation_nonce":binding.creation_nonce,
        })).await;
        let map_requester = |value: &Value| -> Option<String> {
            value.as_str().map(|value| match value {
                "user" => root.did().to_owned(),
                "agent" => agent.did().to_owned(),
                "other" => foreign.did().to_owned(),
                _ => panic!("unknown modeled requester {value}"),
            })
        };
        let session_requester = map_requester(&case.binding["sessionRequester"]);
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let session_doc = create_document(
            &root_access,
            "AgentSession",
            json!({
                "session_id":session_id, "node_did":root.did(), "agent_did":agent.did(),
                "requester_did":session_requester, "created_at":"2026-10-21T00:00:00Z",
            }),
        )
        .await;
        root_access
            .add_actor_relationship(
                "AgentSession",
                &session_doc,
                "working_agent",
                &agent.did().parse().unwrap(),
            )
            .await
            .unwrap();
        let mut parent = AgentRequestCreate::base(
            RequestPurpose::Normal,
            uuid::Uuid::new_v4().to_string(),
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
        let response = root_access
            .write(
                "test.session_child.parent",
                &parent.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
        let parent_doc = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        root_access
            .add_actor_relationship(
                "AgentRequest",
                &parent_doc,
                "working_agent",
                &agent.did().parse().unwrap(),
            )
            .await
            .unwrap();
        let parent = crate::request::admission::load_request_for_admission_test(&node, &parent_doc)
            .await
            .unwrap();
        let purpose = RequestPurpose::try_from(text(&case.binding, "auxiliaryPurpose")).unwrap();
        let aux = crate::request::lifecycle::materialize::write_pending_auxiliary_request(
            &node,
            &parent,
            purpose,
            "audit".to_owned(),
            (purpose == RequestPurpose::AccessReview).then(|| "2030-01-01T00:00:00Z".to_owned()),
            &agent.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap();
        let absent_request_doc = format!("bae-{}", uuid::Uuid::new_v4());
        let actor = if text(&case.binding, "actor") == "agent" {
            &agent
        } else {
            &foreign
        };
        let access = crate::ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(actor.did()).unwrap(),
            actor.as_ref(),
        )
        .unwrap();
        let child_node = if text(&case.binding, "childNode") == "node" {
            root.did()
        } else {
            foreign.did()
        };
        let child_session = if text(&case.binding, "childSession") == "session" {
            session_id.as_str()
        } else {
            "foreign-session"
        };
        let requester = map_requester(&case.binding["childRequester"]);
        let request_doc = if case.name == "missing_auxiliary" {
            &absent_request_doc
        } else if text(&case.binding, "childRequestDoc") == "aux-doc" {
            &aux.doc_id
        } else {
            &parent_doc
        };
        let kind = match text(&case.binding, "source") {
            "title" => CaptureScopeKind::Title,
            "access-review" => CaptureScopeKind::AccessReview,
            "ordinary" => CaptureScopeKind::Inference,
            value => panic!("unknown modeled source {value}"),
        };
        let scope = CaptureScope { kind, seq: 1 };
        let output = OutputSource::ProviderTurn {
            scope,
            turn_index: 0,
            attempt: 0,
        };
        let source = match text(&case.binding, "collection") {
            "AgentOutputSegment" => SessionChildSource::Output(&output),
            "RenderedRequest" => SessionChildSource::Capture(&scope),
            value => panic!("unrepresented collection {value}"),
        };
        let actual = access
            .transact_readonly("test.session_child.binding", |txn| {
                let requester = &requester;
                Box::pin(async move {
                    session_child_binding_for_request_in_txn(
                        txn,
                        child_node,
                        child_session,
                        requester.as_deref(),
                        request_doc,
                        source,
                    )
                    .await
                })
            })
            .await;
        assert_eq!(actual.is_ok(), case.allowed, "{}: {actual:?}", case.name);
        if let Ok((physical_session, selected_agent)) = actual {
            assert_eq!(physical_session, session_doc, "{}", case.name);
            assert_eq!(selected_agent, agent.did(), "{}", case.name);
        }
        node.shutdown().await.unwrap();
    }
    assert_eq!(
        exercised.len(),
        selected.len(),
        "every selected Lean case must be exercised"
    );
}
