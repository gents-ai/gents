use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_p2p::p2p_reconcile::enrollment_authority_channel;
use gents_p2p::p2p_reconcile::enrollment_reconcile::test_support::{
    active_enrollment, projection_with_active, projection_with_revoked, spawn_scripted_authority,
    ScriptedProjection,
};
use gents_p2p::p2p_reconcile::enrollment_store::ActiveEnrollment;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::admission::AgentRequestAdmissionVerifier;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde::Deserialize;

use super::verify_request_at_claim_boundary;

async fn create_enrollment_request(
    node: Arc<EmbeddedNode>,
    target: &dyn NodeIdentity,
    member: &dyn NodeIdentity,
    agent_did: &str,
    active: &ActiveEnrollment,
    label: &str,
) -> gents_protocol::row::AgentRequest {
    let mut create = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        format!("agent-request-{label}"),
        target.did(),
        member.did(),
        agent_did,
        format!("session-{label}"),
        format!("content-{label}"),
        "interactive",
        "2030-01-01T00:00:02Z",
        AgentRequestAdmissionRecord::enrollment(
            member.did(),
            &active.request.request_id,
            &active.request.request_digest,
            &active.request.admin_did,
            active.revision.sequence,
            &active.revision.authorization_expires_at,
        ),
    );
    gents_session::request::admission::sign_agent_request_create(member, &mut create)
        .await
        .expect("sign enrollment AgentRequest");
    // The runtime stores the member's signed request and grants its target
    // Agent the `working_agent` edge it reads and terminalizes through.
    let mutation = create.graphql_mutation().unwrap();
    let working_agent = ::identity::Did::new(agent_did).unwrap();
    let doc_id = runtime_access(&node, target)
        .transact_write("test.enrollment_request", |txn| {
            let (mutation, working_agent) = (&mutation, &working_agent);
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc_id = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &doc_id,
                    "working_agent",
                    working_agent,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc_id)
            })
        })
        .await
        .expect("create enrollment AgentRequest");
    gents_session::request::admission::load_request_for_admission_test_as(&node, &doc_id, agent_did)
        .await
        .expect("load queued AgentRequest")
}

fn runtime_access(
    node: &Arc<EmbeddedNode>,
    target: &dyn NodeIdentity,
) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(target.did()).unwrap(),
    )
}

async fn request_row_as_runtime<T>(
    node: &Arc<EmbeddedNode>,
    target: &dyn NodeIdentity,
    request_id: &str,
) -> T
where
    T: serde::de::DeserializeOwned,
{
    let response = runtime_access(node, target)
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                lifecycle_state claimed_at failure_reason
            }} }}"#,
            gents_store::graphql::escape_graphql_string(request_id),
        ))
        .await
        .expect("query AgentRequest as its runtime");
    serde_json::from_value(response["data"]["AgentRequest"][0].clone())
        .expect("decode AgentRequest row")
}

#[tokio::test]
async fn enrollment_authority_reloads_each_generation_and_cross_actor_claims_stay_blocked() {
    let target_dir = tempfile::tempdir().unwrap();
    let member_dir = tempfile::tempdir().unwrap();
    let target: Arc<dyn NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(target_dir.path().join("target.key"), None).unwrap(),
    );
    let member: Arc<dyn NodeIdentity> = Arc::new(
        KeyIdentity::load_or_create(member_dir.path().join("member.key"), None).unwrap(),
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(target.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(node.as_ref())
        .await
        .unwrap();
    let binding = gents_store::identity::provision_agent_identity(target.as_ref()).unwrap();
    gents_inference::test_support::install_test_agent_binding(
        &node,
        target.did(),
        "behavior-1",
        &binding,
    )
    .await;
    let agent =
        gents_store::identity::resolve_agent_identity(target.as_ref(), &binding).unwrap();

    let mut generation_one = active_enrollment(1, "2030-01-01T00:10:00Z");
    generation_one.request.candidate_did = member.did().to_string();
    generation_one.request.owner_node_did = target.did().to_string();
    generation_one.decision.candidate_did = member.did().to_string();
    generation_one.decision.owner_node_did = target.did().to_string();
    generation_one.revision.member_did = member.did().to_string();
    generation_one.revision.owner_node_did = target.did().to_string();
    let mut expired = generation_one.clone();
    expired.request.request_id = "enrollment-request-expired".into();
    expired.request.request_digest = "enrollment-digest-expired".into();
    expired.decision.request_id = expired.request.request_id.clone();
    expired.decision.request_digest = expired.request.request_digest.clone();
    expired.decision.authorization_expires_at = "2030-01-01T00:05:00Z".into();
    expired.revision.request_id = expired.request.request_id.clone();
    expired.revision.request_digest = expired.request.request_digest.clone();
    expired.revision.authorization_expires_at = "2030-01-01T00:05:00Z".into();
    let mut generation_two = active_enrollment(2, "2030-01-01T00:20:00Z");
    generation_two.request.candidate_did = member.did().to_string();
    generation_two.request.owner_node_did = target.did().to_string();
    generation_two.decision.candidate_did = member.did().to_string();
    generation_two.decision.owner_node_did = target.did().to_string();
    generation_two.revision.member_did = member.did().to_string();
    generation_two.revision.owner_node_did = target.did().to_string();

    let revoked_request = create_enrollment_request(
        node.clone(),
        target.as_ref(),
        member.as_ref(),
        agent.did(),
        &generation_one,
        "revoked",
    )
    .await;
    let expired_request = create_enrollment_request(
        node.clone(),
        target.as_ref(),
        member.as_ref(),
        agent.did(),
        &expired,
        "expired",
    )
    .await;
    let superseded_request = create_enrollment_request(
        node.clone(),
        target.as_ref(),
        member.as_ref(),
        agent.did(),
        &generation_one,
        "superseded",
    )
    .await;
    let replacement_request = create_enrollment_request(
        node.clone(),
        target.as_ref(),
        member.as_ref(),
        agent.did(),
        &generation_two,
        "replacement",
    )
    .await;

    // The authority owner reloads the signed projection on every fresh
    // read: revocation, supersession and a read failure each surface there.
    let (owner, handle) = enrollment_authority_channel();
    let command_task = spawn_scripted_authority(
        owner,
        vec![
            ScriptedProjection::Projection(Box::new(projection_with_active(
                generation_one.clone(),
            ))),
            ScriptedProjection::Projection(Box::new(projection_with_revoked(&generation_one))),
            ScriptedProjection::Projection(Box::new(projection_with_active(
                generation_two.clone(),
            ))),
            ScriptedProjection::Error("injected enrollment projection read failure"),
        ],
    );
    let verifier =
        AgentRequestAdmissionVerifier::new(node.clone(), target.clone(), handle.clone());

    let first_fence = handle
        .fresh_member_authorization(member.did())
        .await
        .unwrap()
        .expect("generation one is active");
    assert_eq!(first_fence.authorization_sequence, 1);
    assert!(handle
        .fresh_member_authorization(member.did())
        .await
        .unwrap()
        .is_none());
    let second_fence = handle
        .fresh_member_authorization(member.did())
        .await
        .unwrap()
        .expect("generation two supersedes generation one");
    assert_eq!(second_fence.authorization_sequence, 2);
    let read_error = handle
        .fresh_member_authorization(member.did())
        .await
        .unwrap_err();
    assert!(format!("{read_error:#}").contains("injected enrollment projection read failure"));
    command_task.await.unwrap();

    // A member-signed request is cross-actor work for the target Agent.
    // Without atomic requester authorization in the claim transaction the
    // modeled decision is blocked (Request.CrossActorAuthorization): it is
    // neither denied nor claimed, so every generation stays pending.
    #[derive(Deserialize)]
    struct PendingRow {
        lifecycle_state: RequestLifecycleState,
        claimed_at: Option<String>,
        failure_reason: Option<String>,
    }
    for request in [
        revoked_request,
        expired_request,
        superseded_request,
        replacement_request,
    ] {
        let request_id = request.request_id.clone();
        let blocked = verifier
            .verify_fresh_at(
                &request,
                agent.did(),
                "2030-01-01T00:06:00Z".parse().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(!blocked.is_denied(), "{request_id}: {blocked:#}");
        assert!(
            format!("{blocked:#}").contains("atomic requester authorization"),
            "{request_id}: {blocked:#}"
        );
        assert!(
            verify_request_at_claim_boundary(
                &verifier,
                node.clone(),
                agent.as_ref(),
                request,
            )
            .await
            .is_none()
        );
        let row: PendingRow = request_row_as_runtime(&node, target.as_ref(), &request_id).await;
        assert_eq!(
            row.lifecycle_state,
            RequestLifecycleState::Pending,
            "{request_id}"
        );
        assert!(row.claimed_at.is_none(), "{request_id}");
        assert!(
            row.failure_reason.as_deref().is_none_or(str::is_empty),
            "{request_id}"
        );
    }
}
