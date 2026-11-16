use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::RequestInput;

use super::{AgentRequestAdmissionError, AgentRequestAdmissionVerifier};
type LeanTitleRequestAdmissionCase =
    gents_lean_vocab::families::contracts_title_admission_fresh_verifier::TitleAdmissionInput;
use gents_p2p::p2p_reconcile::enrollment_authority_channel;
use gents_store::identity::{KeyIdentity, NodeIdentity};

fn native_purpose(purpose: &str) -> RequestPurpose {
    RequestPurpose::try_from(purpose).expect("modeled purpose has a native representation")
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

/// Creates the signed row as the runtime root and grants its working Agent,
/// the relationship every request row carries in production.
async fn insert_request(node: &Arc<EmbeddedNode>, create: &AgentRequestCreate) -> String {
    let root = node
        .node_identity_did()
        .expect("fixture node identity")
        .parse()
        .expect("fixture node DID");
    let mutation = create.graphql_mutation().expect("native request mutation");
    gents_store::config_client::ConfigAccess::local_as(node.clone(), root)
        .transact_write("test.title_admission.create_request", |txn| {
            let mutation = &mutation;
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &doc,
                    "working_agent",
                    &::identity::Did::new(&create.agent_did)?,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .expect("create request")
}

async fn update_request_field(node: &Arc<EmbeddedNode>, doc_id: &str, field: &str, value: &str) {
    assert!(matches!(field, "lifecycle_state" | "deadline"));
    let doc_id = gents_store::graphql::escape_graphql_string(doc_id);
    let value = gents_store::graphql::escape_graphql_string(value);
    let mutation = format!(
        "mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: \"{doc_id}\" }} }}, input: {{ {field}: \"{value}\" }}) {{ _docID }} }}"
    );
    let response = gents_store::config_client::ConfigAccess::write_local_response(
        node,
        "test.title_admission.update_request",
        &mutation,
    )
    .await
    .expect("update request");
    assert!(
        gents_store::graphql::single_mutation_document(&response, "update_AgentRequest")
            .expect("update response")
            .is_some(),
        "request update must match the exact document"
    );
}

/// Maps a modeled agent label to a signing Agent identity. Equal labels map to
/// equal identities and distinct labels to distinct ones, so the model's agent
/// relations are kept.
fn modeled_agent(label: &str) -> Arc<dyn NodeIdentity> {
    let home = tempfile::tempdir().unwrap();
    let root = crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path());
    gents_store::identity::derive_agent_identity(&root, &modeled_nonce(label))
        .expect("derive modeled agent identity")
}

fn modeled_nonce(label: &str) -> String {
    label
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .chain(std::iter::repeat('0'))
        .take(64)
        .collect()
}

fn modeled_binding(label: &str) -> gents_protocol::agent_identity::AgentIdentityBinding {
    gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: modeled_agent_did(label),
        creation_nonce: modeled_nonce(label),
    }
}

fn modeled_agent_did(label: &str) -> String {
    modeled_agent(label).did().to_owned()
}

async fn verify_generated_case(case: &LeanTitleRequestAdmissionCase) -> String {
    let temp = tempfile::tempdir().expect("temporary identity directory");
    crate::request::lifecycle::test_support::pin_fixed_signing_identity(temp.path());
    let node_db = gents_inference::test_support::test_db_as(
        "title-generated-case-node",
        crate::request::lifecycle::test_support::PIN_FIXED_DID,
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let identity = Arc::new(gents_store::test_support::held_runtime_identity(
        node_db.node_identity.did(),
    ));
    let foreign = KeyIdentity::load_or_create(temp.path().join("foreign.key"), None)
        .expect("foreign identity");

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
    let mut labels = vec![
        case.session_agent.as_str(),
        case.request.agent_did.as_str(),
        parent.agent_did.as_str(),
    ];
    labels.sort_unstable();
    labels.dedup();
    for label in labels {
        gents_inference::test_support::install_test_agent_binding(
            &node,
            target,
            label,
            &modeled_binding(label),
        )
        .await;
    }
    let request_agent = modeled_agent(&case.request.agent_did);
    // The modeled requester is the request's own Agent, the target node, or a
    // foreign principal.
    let requester = if case.request.requester_did == case.request.agent_did {
        request_agent.did()
    } else if case.request.requester_did == case.request.target_node {
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
    // Each request is signed by its own Agent: the model's target runtime is
    // the Agent that executes the request on this node.
    let parent_agent = modeled_agent(&parent.agent_did);
    let mut parent_create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        parent_request_id,
        target,
        parent_agent.did(),
        parent_agent.did(),
        &parent.session_id,
        &case.request.content,
        "interactive",
        &case.request.created_at,
        AgentRequestAdmissionRecord::local_self(parent_agent.did()),
    );
    super::sign_agent_request_create(parent_agent.as_ref(), &mut parent_create)
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
                request_agent.did(),
                &case.admission.source_request_id,
            )
        }
        ("runtime-internal", "automated-trigger") => {
            AgentRequestAdmissionRecord::runtime_automated_trigger(
                request_agent.did(),
                &case.admission.source_request_id,
            )
        }
        ("local-self", _) => AgentRequestAdmissionRecord::local_self(request_agent.did()),
        other => panic!("unsupported generated admission branch: {other:?}"),
    };
    let mut create = AgentRequestCreate::base(
        native_purpose(&case.request.purpose),
        &case.request.request_id,
        target,
        requester,
        request_agent.did(),
        &case.request.session_id,
        &case.request.content,
        "interactive",
        &case.request.created_at,
        admission,
    );
    create.max_retries = 0;
    create.input = native_input(case);
    create.request_hop = u32::try_from(case.request.hop).unwrap();
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
        super::sign_agent_request_create(request_agent.as_ref(), &mut create)
            .await
            .expect("sign before tampering");
        create.content.push_str(&case.request.request_id);
    } else {
        super::sign_agent_request_create(request_agent.as_ref(), &mut create)
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
    let result = verifier
        .verify_fresh(&queued, &modeled_agent_did(&case.session_agent))
        .await;
    let actual_disposition = match &result {
        Ok(_) => "admit",
        Err(AgentRequestAdmissionError::Denied(_)) => "deny",
        Err(AgentRequestAdmissionError::Unavailable(_)) => "retry",
    };
    if let Ok(admitted) = &result {
        assert_eq!(admitted.doc_id, doc_id);
        assert_eq!(admitted.request_id, case.request.request_id);
        assert_eq!(admitted.purpose, native_purpose(&case.request.purpose));
    }
    node.shutdown().await.expect("shut down test node");
    actual_disposition.to_owned()
}

#[tokio::test]
async fn generated_title_admission_cases_bind_representable_fresh_verifier_subset() {
    for case in gents_lean_vocab::families::contracts_title_admission_fresh_verifier::cases() {
        let disposition = verify_generated_case(&case.input).await;
        case.check(disposition);
    }
}
