use anyhow::Result;
use chrono::Utc;
use gents_documents::skills::prompt_slash_skill_selection;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use gents_session::request::lifecycle::{ExecutionOrigin, DEFAULT_REQUEST_MAX_RETRIES};
use gents_session::request::lifecycle::{RequestIdentity, RequestSigner, RequestSpec, RetryLink};
use gents_store::config_client::ConfigAccess;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::super::graphql::normalize_required;
use super::super::binding::resolve_agent_binding;
use super::{SubmitRequestOptions, SubmittedRequest};

fn normalize_authored_content<'a>(
    input: &RequestInput,
    content: &'a str,
) -> Result<&'a str> {
    anyhow::ensure!(
        input.has_authored_input(content),
        "content must not be empty"
    );
    Ok(content.trim())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_request_submission(
    access: &ConfigAccess,
    store: &ClientStore,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents_store::identity::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_did: Option<&str>,
    options: SubmitRequestOptions,
    request_id: Option<&str>,
) -> Result<(
    SubmittedRequest,
    gents_protocol::request_admission::AgentRequestCreate,
)> {
    let session_id = normalize_required("session_id", session_id)?;
    let node_did = normalize_required("node_did", node_did)?;
    let requester_did = normalize_required("requester_did", requester_did)?;
    let content = normalize_authored_content(&options.input, content)?.to_owned();
    let (content, options) = prepare_prompt_submission(&content, options)?;
    let request_id = request_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let binding = resolve_agent_binding(store, node_did, agent_did, Some(session_id))?;
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let create = gents_session::request::lifecycle::build_signed_client_request(
        access,
        RequestSpec {
            retry: Some(RetryLink {
                parent_request_id: None,
                parent_request_doc_id: None,
                root_request_id: request_id.clone(),
                retry_count: 0,
                max_retries: i64::from(DEFAULT_REQUEST_MAX_RETRIES),
            }),
            input: options.input,
            trigger_lineage: options.trigger_lineage,
            // The owner's idempotency key: a resend of this identity is the
            // same request (Lean `ClientOptimisticSend.create`).
            retry_key: Some(options.retry_key.unwrap_or_else(|| {
                gents_session::request::lifecycle::client_request_key(signer.did(), &request_id)
            })),
            ..RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                RequestIdentity {
                    request_id: request_id.clone(),
                    node_did: node_did.to_string(),
                    requester_did: Some(requester_did.to_string()),
                    agent_did: binding.agent_did.clone().unwrap_or_default(),
                    session_id: session_id.to_string(),
                    content: content.to_string(),
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at,
                },
                admission,
            )
        },
        RequestSigner::Identity(signer),
    )
    .await?;
    Ok((
        SubmittedRequest {
            request_id,
            session_id: session_id.to_string(),
            node_did: node_did.to_string(),
            agent_did: binding.agent_did,
        },
        create,
    ))
}

fn prepare_prompt_submission(
    content: &str,
    mut options: SubmitRequestOptions,
) -> Result<(String, SubmitRequestOptions)> {
    let selection = prompt_slash_skill_selection(content);
    if selection.selected_skill_ids.is_empty() {
        return Ok((content.to_string(), options));
    }

    options.input.selected_skill_ids = selection.selected_skill_ids;
    Ok((selection.prompt, options))
}
