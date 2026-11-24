use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
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

/// The enrollment field a runtime-internal admission must not carry; a peer
/// writing the row directly can set it although the native create refuses it.
const FOREIGN_BRANCH_FIELD: &str = "foreign-enrollment-request";

/// Creates the signed row as the runtime root and grants its working Agent,
/// the relationship every request row carries in production.
async fn insert_request(node: &Arc<EmbeddedNode>, create: &AgentRequestCreate) -> String {
    let root = node
        .node_identity_did()
        .expect("fixture node identity")
        .parse()
        .expect("fixture node DID");
    let mutation = match &create.admission.enrollment_request_id {
        None => create.graphql_mutation().expect("native request mutation"),
        Some(foreign) => {
            let mut canonical = create.clone();
            canonical.admission.enrollment_request_id = None;
            canonical
                .graphql_mutation()
                .expect("native request mutation")
                .replacen(
                    "create_AgentRequest(input: { ",
                    &format!(
                        "create_AgentRequest(input: {{ enrollment_request_id: \"{}\", ",
                        gents_store::graphql::escape_graphql_string(foreign)
                    ),
                    1,
                )
        }
    };
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
    let root = gents_store::test_support::pin_fixed_signing_identity(home.path());
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

/// The row and world facts one replay varies around an admission case: the
/// fresh-verifier cases take them from the case, the join cases override them.
struct Variation {
    observation_available: bool,
    branch_fields_exact: bool,
    pending_deadline_absent: bool,
    signed_purpose: String,
    signature_valid: bool,
    observed_request_id: String,
    parent_state: String,
    claim: bool,
}

impl Variation {
    fn of(case: &LeanTitleRequestAdmissionCase) -> Self {
        Self {
            observation_available: case.observation_available,
            branch_fields_exact: case.branch_fields_exact,
            pending_deadline_absent: case.pending_deadline_absent,
            signed_purpose: model_text_fields(&case.admission.model_signed_fields_hex)[2].clone(),
            signature_valid: case.admission.signature_valid,
            observed_request_id: case.request.request_id.clone(),
            parent_state: case.parent_observed_state.clone(),
            claim: false,
        }
    }
}

/// What the claim owner did with an admitted request.
struct ClaimObservation {
    claimed: bool,
    lease_active: bool,
    queue_unchanged: bool,
}

struct Replay {
    disposition: String,
    purpose: Option<RequestPurpose>,
    claim: Option<ClaimObservation>,
}

async fn verify_generated_case(case: &LeanTitleRequestAdmissionCase) -> String {
    replay_case(case, Variation::of(case)).await.disposition
}

async fn lifecycle_state(node: &Arc<EmbeddedNode>, doc_id: &str) -> Option<String> {
    super::load_signed_request(node.as_ref(), doc_id)
        .await
        .expect("read request state")
        .lifecycle_state
        .map(|state| state.as_str().to_owned())
}

async fn replay_case(case: &LeanTitleRequestAdmissionCase, variation: Variation) -> Replay {
    let temp = tempfile::tempdir().expect("temporary identity directory");
    gents_store::test_support::pin_fixed_signing_identity(temp.path());
    let node_db = gents_store::test_support::TestStore::start_as(
        "title-generated-case-node",
        gents_store::test_support::PIN_FIXED_DID,
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
    // Every request runs in a session its claimant reads, with that session's
    // interrupt marker (`InterruptGeneration.claimRecord`).
    {
        let session_agent = modeled_agent(&case.session_agent);
        let parent_agent = modeled_agent(&parent.agent_did);
        let root: ::identity::Did = target.parse().expect("fixture node DID");
        gents_store::config_client::ConfigAccess::local_as(node.clone(), root)
            .transact_write("test.title_admission.session", |txn| {
                let (session_agent, parent_agent, request_agent) =
                    (&session_agent, &parent_agent, &request_agent);
                Box::pin(async move {
                    crate::session::ensure_session_in_txn(
                        txn,
                        crate::session::NewAgentSession {
                            session_id: &case.request.session_id,
                            node_did: target,
                            agent_did: session_agent.did(),
                            requester_did: Some(parent_agent.did()),
                            title: None,
                            provenance: None,
                            created_at: &case.request.created_at,
                        },
                    )
                    .await?;
                    let session =
                        gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                            txn,
                            target,
                            &case.request.session_id,
                        )
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("fixture session"))?;
                    txn.add_actor_relationship(
                        "AgentSession",
                        &session.doc_id,
                        "working_agent",
                        &::identity::Did::new(request_agent.did())?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .expect("create the modeled session");
    }
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
    let write_parent = |request_id: String| {
        let node = node.clone();
        let session = parent.session_id.clone();
        let agent = parent.agent_did.clone();
        let content = case.request.content.clone();
        let created_at = case.request.created_at.clone();
        let state = variation.parent_state.clone();
        async move {
            // Each request is signed by its own Agent: the model's target
            // runtime is the Agent that executes the request on this node.
            let parent_agent = modeled_agent(&agent);
            let mut create = AgentRequestCreate::base(
                RequestPurpose::Normal,
                request_id,
                RequestScope::new(target, parent_agent.did(), parent_agent.did(), &session),
                &content,
                "interactive",
                &created_at,
                AgentRequestAdmissionRecord::local_self(parent_agent.did()),
            );
            super::sign_agent_request_create(parent_agent.as_ref(), &mut create)
                .await
                .expect("sign physical parent");
            let written = insert_request(&node, &create).await;
            update_request_field(&node, &written, "lifecycle_state", &state).await;
            written
        }
    };
    let parent_doc_id = write_parent(parent_request_id).await;
    // A parent document label other than the evidenced parent's names another
    // normal request of the same session, which does not own the source.
    let linked_doc_id = if parent_fields[3] == parent.document_id {
        parent_doc_id.clone()
    } else {
        write_parent(parent_fields[3].clone()).await
    };

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
        RequestScope::new(
            target,
            requester,
            request_agent.did(),
            &case.request.session_id,
        ),
        &case.request.content,
        "interactive",
        &case.request.created_at,
        admission,
    );
    create.input = native_input(case);
    create.request_hop = u32::try_from(case.request.hop).unwrap();
    create.caused_by_parent_request_id = Some(parent_fields[1].clone());
    create.caused_by_parent_request_doc_id = Some(linked_doc_id);
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
    create.purpose = RequestPurpose::try_from(variation.signed_purpose.as_str())
        .expect("modeled signed purpose has a native representation");
    if !variation.branch_fields_exact {
        create.admission.enrollment_request_id = Some(FOREIGN_BRANCH_FIELD.to_owned());
    }
    let signed = super::sign_agent_request_create(request_agent.as_ref(), &mut create).await;
    if variation.branch_fields_exact {
        signed.expect("sign modeled title request");
    } else {
        signed.expect_err("the native signer refuses foreign branch fields after signing them");
    }
    if !variation.signature_valid {
        create.content.push_str(&case.request.request_id);
    }

    create.purpose = actual_purpose;
    let doc_id = insert_request(&node, &create).await;
    let row = super::load_signed_request(node.as_ref(), &doc_id)
        .await
        .unwrap_or_else(|error| panic!("{}: load queued request: {error:#}", case.name));
    // The watcher rejects at ingest a pending row its typed request refuses,
    // so such a row is denied before admission runs.
    let Ok(mut queued) = gents_protocol::row::AgentRequest::try_from(row) else {
        node.shutdown().await.expect("shut down test node");
        return Replay {
            disposition: "deny".to_owned(),
            purpose: None,
            claim: None,
        };
    };
    queued.request_id = variation.observed_request_id.clone();
    if !variation.pending_deadline_absent {
        update_request_field(&node, &doc_id, "deadline", &case.request.created_at).await;
    }
    let (_owner, enrollment) = enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), identity, enrollment);
    let parent_state_before = lifecycle_state(&node, &parent_doc_id).await;
    let session_agent = modeled_agent_did(&case.session_agent);
    let verification = verifier.verify_fresh(&queued, &session_agent);
    let result = if variation.observation_available {
        verification.await
    } else {
        gents_store::config_client::ConfigApplyTxn::with_read_storage_failure(
            "AgentRequest",
            verification,
        )
        .await
    };
    let actual_disposition = match &result {
        Ok(_) => "admit",
        Err(AgentRequestAdmissionError::Denied(_)) => "deny",
        Err(AgentRequestAdmissionError::Unavailable(_)) => "retry",
    };
    let mut replay = Replay {
        disposition: actual_disposition.to_owned(),
        purpose: None,
        claim: None,
    };
    if let Ok(admitted) = result {
        assert_eq!(admitted.doc_id, doc_id);
        assert_eq!(admitted.request_id, case.request.request_id);
        assert_eq!(admitted.purpose, native_purpose(&case.request.purpose));
        replay.purpose = Some(admitted.purpose);
        if variation.claim {
            let mut lifecycle =
                crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                    node.clone(),
                    admitted,
                    60,
                    crate::request::lifecycle::ExecutionOrigin::Interactive,
                    String::new(),
                );
            let home = tempfile::tempdir().unwrap();
            let root = gents_store::test_support::pin_fixed_signing_identity(home.path());
            let managed = gents_store::identity::resolve_agent_identity(
                &root,
                &modeled_binding(&case.request.agent_did),
            )
            .expect("resolve the request's managed Agent");
            lifecycle
                .bind_managed_agent(managed.as_ref())
                .expect("bind the request's managed Agent");
            let claimed = matches!(
                lifecycle.claim_with_identity().await,
                Ok(crate::request::lifecycle::ClaimOutcome::Claimed)
            );
            replay.claim = Some(ClaimObservation {
                claimed,
                lease_active: lifecycle_state(&node, &doc_id).await.as_deref() == Some("claimed"),
                queue_unchanged: lifecycle_state(&node, &parent_doc_id).await
                    == parent_state_before,
            });
        }
    }
    node.shutdown().await.expect("shut down test node");
    replay
}

#[tokio::test]
async fn generated_title_admission_cases_bind_the_fresh_verifier() {
    for case in gents_lean_vocab::families::contracts_title_admission_fresh_verifier::cases() {
        let disposition = verify_generated_case(&case.input).await;
        case.check(disposition);
    }
}

#[tokio::test]
async fn generated_title_join_cases_bind_admission_and_claim() {
    use gents_lean_vocab::families::title_admission_join::JoinObservation;
    for case in gents_lean_vocab::families::title_admission_join::cases::<String>() {
        let join = &case.input;
        let admission: LeanTitleRequestAdmissionCase = gents_lean_vocab::family::recode(
            join.admission
                .as_ref()
                .expect("a join case names an admission case"),
        );
        let admission = &admission;
        let signed_purpose = match join.row_signed_for.as_str() {
            "title" => "title-audit",
            "access_review" => "access-review",
            _ => admission.request.purpose.as_str(),
        };
        let variation = Variation {
            observation_available: join.observation_available,
            branch_fields_exact: join.branch_fields_exact,
            pending_deadline_absent: join.pending_deadline_absent,
            signed_purpose: signed_purpose.to_owned(),
            signature_valid: join.row_signed_for != "nothing",
            observed_request_id: if join.row_logical_request == admission.request.request_id {
                admission.request.request_id.clone()
            } else {
                join.row_logical_request.clone()
            },
            parent_state: if join.active_normal {
                "processing".to_owned()
            } else {
                admission.parent_observed_state.clone()
            },
            claim: true,
        };
        let replay = replay_case(admission, variation).await;
        let claim = replay.claim.unwrap_or(ClaimObservation {
            claimed: false,
            lease_active: false,
            queue_unchanged: true,
        });
        case.check(JoinObservation {
            activated_purpose: replay.purpose.map(|purpose| purpose.as_str().to_owned()),
            claimed: claim.claimed,
            lease_active: claim.lease_active,
            queue_unchanged: claim.queue_unchanged,
        });
    }
}
