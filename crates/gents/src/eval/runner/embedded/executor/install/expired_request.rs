use anyhow::{Context, Result};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;
use serde_json::Value;

use crate::eval::runner::embedded::home::EmbeddedHome;
use crate::request::lifecycle::{
    build_signed_client_request, submit_client_request, ClaimOutcome, ExecutionOrigin,
    RequestIdentity, RequestLifecycle, RequestSigner, RequestSpec,
};
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpiredClientRequest {
    node_did: String,
    requester_did: String,
    agent_did: String,
    session_id: String,
    request_id: String,
    purpose: RequestPurpose,
    lifecycle_state: RequestLifecycleState,
    created_at: String,
    valid_until: String,
}

pub(super) async fn install(
    access: &ConfigAccess,
    document: &Value,
    home: &EmbeddedHome,
    evaluator: &dyn NodeIdentity,
    agent_identity: &dyn NodeIdentity,
) -> Result<()> {
    let trial_did = home.did();
    let fixture: ExpiredClientRequest = serde_json::from_value(document.clone())
        .context("decoding expired client request fixture")?;
    let actor = access
        .actor_did()
        .context("expired request fixture requires a client actor")?;
    anyhow::ensure!(
        fixture.node_did == trial_did && fixture.requester_did == actor && evaluator.did() == actor,
        "expired request fixture must use the trial node and its client actor"
    );
    anyhow::ensure!(
        fixture.purpose == RequestPurpose::Normal
            && fixture.lifecycle_state == RequestLifecycleState::Dead,
        "expired request fixture must declare a dead normal request"
    );
    let created = chrono::DateTime::parse_from_rfc3339(&fixture.created_at)?;
    let expiry = chrono::DateTime::parse_from_rfc3339(&fixture.valid_until)?;
    anyhow::ensure!(
        created < expiry && expiry < chrono::Utc::now(),
        "fixture request must have expired after its creation"
    );
    let mut spec = RequestSpec::new(
        fixture.purpose,
        RequestIdentity {
            request_id: fixture.request_id.clone(),
            node_did: fixture.node_did.clone(),
            requester_did: Some(fixture.requester_did),
            agent_did: fixture.agent_did.clone(),
            session_id: fixture.session_id,
            content: String::new(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: fixture.created_at,
        },
        AgentRequestAdmissionRecord::client(&actor),
    );
    spec.valid_until = Some(fixture.valid_until);
    let create =
        build_signed_client_request(access, spec, RequestSigner::Identity(evaluator)).await?;
    submit_client_request(access, &create).await?;
    let node = home.node.clone();
    let agent = ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(&fixture.agent_did)?,
        agent_identity,
    )?;
    let response = agent
        .execute(&format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}, limit: 2) {{ {} }} }}",
            escape_graphql_string(&fixture.request_id),
            crate::SIGNED_REQUEST_FIELDS
        ))
        .await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("expired request is not visible to its selected agent")?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("expired request must have one visible physical document")
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
    crate::verify_request_receipt_signature(&row)?;
    let request = gents_protocol::row::AgentRequest::try_from(row)?;
    let mut lifecycle =
        RequestLifecycle::new_with_node_did(node, &fixture.agent_did, trial_did, request, 60);
    lifecycle.bind_managed_agent(agent_identity)?;
    anyhow::ensure!(
        matches!(lifecycle.claim().await?, ClaimOutcome::Expired),
        "fixture request did not expire before claim"
    );
    Ok(())
}
