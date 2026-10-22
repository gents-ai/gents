use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::RequestInput;

use super::{AgentRequestAdmissionError, AgentRequestAdmissionVerifier};
use crate::agent::p2p_reconcile::enrollment_authority_channel;
use crate::identity::{KeyIdentity, NodeIdentity};
use crate::lean_vocab_test::{
    lean_title_request_admission_cases, LeanRequestPurpose, LeanTitleRequestAdmissionCase,
};
use crate::schema::ensure_runtime_schemas;

fn native_purpose(purpose: LeanRequestPurpose) -> RequestPurpose {
    purpose.into()
}

fn model_text_fields(hex_fields: &[String]) -> Vec<String> {
    hex_fields
        .iter()
        .map(|hex| {
            assert_eq!(
                hex.len() % 2,
                0,
                "model byte field must have even hex length"
            );
            let bytes = hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|digits| {
                    u8::from_str_radix(std::str::from_utf8(digits).unwrap(), 16)
                        .expect("model byte field is hex")
                })
                .collect::<Vec<_>>();
            String::from_utf8(bytes).expect("model title fields are text")
        })
        .collect()
}

fn native_input(case: &LeanTitleRequestAdmissionCase) -> RequestInput {
    let modeled = &case.request.input;
    serde_json::from_value(serde_json::json!({
        "selected_skill_ids": modeled.selected_skill_ids,
        "cwd": modeled.cwd,
        "initial_title": modeled.initial_title.as_ref().map(|title| serde_json::json!({
            "text": title.text,
            "source": title.source,
        })),
        "queue": modeled.queue.as_ref().map(|queue| serde_json::json!({
            "delivery": queue.delivery,
            "position": queue.position,
            "source": queue.source,
            "policy": queue.policy,
            "key": queue.key,
            "queued_after_request_id": queue.queued_after_request_id,
            "interrupted_request_id": queue.interrupted_request_id,
            "background_completion_wake_version": queue.background_completion_wake_version,
        })),
        "goal_continuation": modeled.goal_continuation.as_ref().map(|goal| serde_json::json!({
            "sequence": goal.sequence,
            "wrapup": goal.wrapup,
        })),
    }))
    .expect("generated title input has native representation")
}

async fn insert_request(node: &Arc<EmbeddedNode>, create: &AgentRequestCreate) -> String {
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "test.title_admission.create_request",
        &create.graphql_mutation().expect("native request mutation"),
    )
    .await
    .expect("create request");
    crate::graphql::single_mutation_document(&response, "create_AgentRequest")
        .expect("create response")
        .expect("created request")["_docID"]
        .as_str()
        .expect("created physical request ID")
        .to_owned()
}

async fn update_request_field(node: &Arc<EmbeddedNode>, doc_id: &str, field: &str, value: &str) {
    assert!(matches!(field, "lifecycle_state" | "deadline"));
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let value = crate::graphql::escape_graphql_string(value);
    let mutation = format!(
        "mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: \"{doc_id}\" }} }}, input: {{ {field}: \"{value}\" }}) {{ _docID }} }}"
    );
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "test.title_admission.update_request",
        &mutation,
    )
    .await
    .expect("update request");
    assert!(
        crate::graphql::single_mutation_document(&response, "update_AgentRequest")
            .expect("update response")
            .is_some(),
        "request update must match the exact document"
    );
}

async fn verify_generated_case(case: &LeanTitleRequestAdmissionCase) {
    let temp = tempfile::tempdir().expect("temporary identity directory");
    let identity = Arc::new(
        KeyIdentity::load_or_create(temp.path().join("target.key"), None).expect("target identity"),
    );
    let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None)
        .expect("foreign identity");
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .expect("embedded node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("canonical schema baseline");

    let evidence = case
        .runtime_evidence
        .as_ref()
        .expect("modeled runtime evidence");
    let parent = evidence
        .title_parent
        .as_ref()
        .expect("modeled parent evidence");
    assert_eq!(evidence.target_node, case.request.target_node);
    assert_eq!(evidence.source_request_id, parent.request_id);
    let parent_fields = model_text_fields(&case.request.model_parent_fields_hex);
    assert!(
        matches!(parent_fields.len(), 6 | 8),
        "title parent tuple shape"
    );
    assert_eq!(parent_fields[1], parent.request_id);
    assert_eq!(parent_fields[3], parent.document_id);
    let target = identity.did();
    let requester = if case.request.requester_did == case.request.target_node {
        target
    } else {
        foreign.did()
    };

    let stale_parent = !parent.physical_binding_current;
    let parent_request_id = if stale_parent {
        format!("{}-stale", parent.request_id)
    } else {
        parent.request_id.clone()
    };
    let mut parent_create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        parent_request_id,
        target,
        target,
        &parent.agent_id,
        &parent.session_id,
        &case.request.content,
        "interactive",
        &case.request.created_at,
        AgentRequestAdmissionRecord::local_self(target),
    );
    super::sign_agent_request_create(identity.as_ref(), &mut parent_create)
        .await
        .expect("sign physical parent");
    let parent_doc_id = insert_request(&node, &parent_create).await;
    update_request_field(
        &node,
        &parent_doc_id,
        "lifecycle_state",
        &case.parent_observed_state,
    )
    .await;

    let admission = match (
        case.admission.kind.as_str(),
        case.admission.runtime_source_kind.as_str(),
    ) {
        ("runtime-internal", "local-control") => {
            AgentRequestAdmissionRecord::runtime_local_control(
                target,
                &case.admission.source_request_id,
            )
        }
        ("runtime-internal", "automated-trigger") => {
            AgentRequestAdmissionRecord::runtime_automated_trigger(
                target,
                &case.admission.source_request_id,
            )
        }
        ("local-self", _) => AgentRequestAdmissionRecord::local_self(target),
        other => panic!("unsupported generated admission branch: {other:?}"),
    };
    let mut create = AgentRequestCreate::base(
        native_purpose(case.request.purpose),
        &case.request.request_id,
        target,
        requester,
        &case.request.agent_id,
        &case.request.session_id,
        &case.request.content,
        "interactive",
        &case.request.created_at,
        admission,
    );
    create.max_retries = 0;
    create.input = native_input(case);
    create.request_hop = case.request.hop;
    create.caused_by_parent_request_id = Some(parent_fields[1].clone());
    create.caused_by_parent_request_doc_id = Some(parent_doc_id);
    if parent_fields[4] == "some" {
        assert_eq!(parent_fields.len(), 8);
        assert_eq!(parent_fields[6], "some");
        create.caused_by_parent_tool_call_id = Some(parent_fields[5].clone());
        create.caused_by_parent_tool_call_doc_id = Some(parent_fields[7].clone());
    } else {
        assert_eq!(parent_fields.len(), 6);
        assert_eq!(parent_fields[4], "none");
        assert_eq!(parent_fields[5], "none");
    }
    if !case.request.model_retry_fields_hex.is_empty() {
        create.retry_parent_request =
            Some(model_text_fields(&case.request.model_retry_fields_hex).join(""));
    }
    if !case.request.model_trigger_fields_hex.is_empty() {
        create.caused_by_trigger_id =
            Some(model_text_fields(&case.request.model_trigger_fields_hex).join(""));
    }

    let actual_purpose = create.purpose;
    let signed_fields = model_text_fields(&case.admission.model_signed_fields_hex);
    create.purpose = RequestPurpose::try_from(signed_fields[2].as_str())
        .expect("modeled signed purpose has a native representation");
    if !case.admission.signature_valid {
        super::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .expect("sign before tampering");
        create.content.push_str(&case.request.request_id);
    } else {
        super::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .expect("sign modeled title request");
    }

    create.purpose = actual_purpose;
    let doc_id = insert_request(&node, &create).await;
    let queued = super::load_request_for_admission_test(node.as_ref(), &doc_id)
        .await
        .unwrap_or_else(|error| panic!("{}: load queued request: {error:#}", case.name));
    if !case.pending_deadline_absent {
        update_request_field(&node, &doc_id, "deadline", &case.request.created_at).await;
    }
    let (_owner, enrollment) = enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), identity, enrollment);
    let result = verifier.verify_fresh(&queued, &case.session_agent).await;
    let actual_disposition = match &result {
        Ok(_) => "admit",
        Err(AgentRequestAdmissionError::Denied(_)) => "deny",
        Err(AgentRequestAdmissionError::Unavailable(_)) => "retry",
    };
    assert_eq!(
        actual_disposition, case.expected_disposition,
        "{}: native verifier result {result:?}",
        case.name
    );
    if let Ok(admitted) = result {
        assert_eq!(admitted.doc_id, doc_id);
        assert_eq!(admitted.request_id, case.request.request_id);
        assert_eq!(admitted.purpose, native_purpose(case.request.purpose));
    }
    node.shutdown().await;
}

#[tokio::test]
async fn generated_title_admission_cases_bind_representable_fresh_verifier_subset() {
    let mut covered = 0;
    let mut unbound = 0;
    for case in lean_title_request_admission_cases() {
        match case.name.as_str() {
            "ordinary-signed-control-preserved"
            | "pending-title-observation-unavailable"
            | "foreign-admission-branch-fields"
            | "forged-physical-parent"
            | "tool-linked-parent"
            | "signed-purpose-mutation"
            | "missing-parent-evidence"
            | "access-review-observation-unavailable" => unbound += 1,
            "valid-access-review-parent-processing"
            | "access-review-purpose-signature-replay"
            | "access-review-stale-parent-document"
            | "access-review-parent-session-mismatch"
            | "access-review-queue-control-forbidden"
            | "access-review-local-self-forbidden"
            | "valid-title-parent-completed"
            | "valid-title-parent-processing"
            | "present-preclaim-deadline"
            | "forged-requester"
            | "queue-control-input"
            | "skill-control-input"
            | "cwd-control-input"
            | "goal-control-input"
            | "caller-title-control-input"
            | "retry-control-input"
            | "trigger-control-input"
            | "parent-session-mismatch"
            | "stale-parent-document"
            | "unsigned-title"
            | "title-local-self-forbidden"
            | "title-cross-source-forbidden" => {
                verify_generated_case(case).await;
                covered += 1;
            }
            other => panic!("unclassified generated title admission case: {other}"),
        }
    }
    assert_eq!(covered, 22, "all representable generated cases must run");
    assert_eq!(unbound, 8, "unbound cases must remain explicit");
}
