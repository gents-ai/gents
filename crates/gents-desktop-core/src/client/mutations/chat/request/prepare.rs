use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents::documents::skills::prompt_slash_skill_selection;
use gents::lifecycle::{ExecutionOrigin, TriggerLineage, DEFAULT_REQUEST_MAX_RETRIES};
use gents::{build_signed_request, RequestIdentity, RequestSigner, RequestSpec, RetryLink};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use serde_json::Value;
use uuid::Uuid;

use crate::client::store::ClientStore;

use super::super::super::graphql::{
    escape_graphql_string, normalize_optional_string, normalize_required,
};
use super::super::binding::resolve_agent_binding;
use super::{SubmitRequestOptions, SubmittedRequest};

pub(super) fn normalize_authored_content<'a>(
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
    node: &EmbeddedNode,
    store: &ClientStore,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_id: Option<&str>,
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
    let binding = resolve_agent_binding(store, node_did, agent_id, Some(session_id))?;
    if let Some(mailbox_item_id) = options
        .caused_by_source_doc_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        validate_mailbox_submission_cause(
            node,
            mailbox_item_id,
            requester_did,
            node_did,
            binding.agent_id.as_deref().unwrap_or(""),
            session_id,
        )
        .await?;
    }

    let (retry_parent_request, retry_parent_request_doc_id, retry_root_request, parent_created_at) =
        if let Some(parent_id) = options.retry_parent_request.as_deref() {
            let parent = fetch_retry_lineage(node, parent_id, node_did, requester_did).await?;
            (
                parent_id.to_string(),
                Some(parent.doc_id),
                parent
                    .retry_root_request
                    .unwrap_or_else(|| parent_id.to_string()),
                parent.created_at,
            )
        } else {
            (String::new(), None, request_id.clone(), None)
        };
    let created_at = canonical_request_created_at_after(parent_created_at.as_deref())?;

    let create = build_signed_request(
        RequestSpec {
            retry: Some(RetryLink {
                parent_request_id: (!retry_parent_request.is_empty())
                    .then_some(retry_parent_request),
                parent_request_doc_id: retry_parent_request_doc_id,
                root_request_id: retry_root_request,
                retry_count: 0,
                max_retries: i64::from(DEFAULT_REQUEST_MAX_RETRIES),
            }),
            input: options.input,
            valid_until: options
                .valid_until
                .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            trigger_lineage: TriggerLineage {
                source_doc_id: options
                    .caused_by_source_doc_id
                    .or(options.trigger_lineage.source_doc_id),
                ..options.trigger_lineage
            },
            retry_key: options.retry_key,
            ..RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                RequestIdentity {
                    request_id: request_id.clone(),
                    node_did: node_did.to_string(),
                    requester_did: Some(requester_did.to_string()),
                    agent_id: binding.agent_id.clone().unwrap_or_default(),
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
            agent_id: binding.agent_id,
        },
        create,
    ))
}

pub(super) async fn validate_mailbox_submission_cause(
    node: &EmbeddedNode,
    item_id: &str,
    requester_did: &str,
    node_did: &str,
    agent_id: &str,
    session_id: &str,
) -> Result<()> {
    let query = format!(
        r#"query {{
            MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{
                requester_did status action target_node_did target_agent_id session_id
            }}
        }}"#,
        escape_graphql_string(item_id)
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "validate mailbox submission cause",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("MailboxItem"))
        .and_then(Value::as_array)
        .context("mailbox cause query returned no row array")?;
    if rows.len() != 1 {
        bail!("mailbox submission cause must identify exactly one item");
    }
    let row = &rows[0];
    let field = |name: &str| row.get(name).and_then(Value::as_str).unwrap_or("");
    if field("requester_did") != requester_did {
        bail!("mailbox submission cause is owned by another requester");
    }
    if field("status") != "open" {
        bail!("mailbox submission cause is no longer open");
    }
    if !matches!(field("action"), "start_request" | "write_document") {
        bail!("mailbox submission cause does not open a compose surface");
    }
    if field("target_node_did") != node_did || field("target_agent_id") != agent_id {
        bail!("mailbox submission route does not match the target agent");
    }
    if let Some(expected_session) = row
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if expected_session != session_id {
            bail!("mailbox submission route does not match the target session");
        }
    }
    Ok(())
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

struct RetryLineage {
    doc_id: String,
    retry_root_request: Option<String>,
    created_at: Option<String>,
}

async fn fetch_retry_lineage(
    node: &EmbeddedNode,
    request_id: &str,
    node_did: &str,
    requester_did: &str,
) -> Result<RetryLineage> {
    let escaped = escape_graphql_string(request_id);
    let node_did = escape_graphql_string(node_did);
    let requester_did = escape_graphql_string(requester_did);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{escaped}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    requester_did: {{ _eq: "{requester_did}" }}
                }},
                limit: 2
            ) {{
                _docID
                retry_root_request
                created_at
            }}
        }}"#
    );
    let resp = gents::graphql::graphql_with_transaction_retry(node, &query, "fetch_retry_lineage")
        .await
        .with_context(|| format!("fetch_retry_lineage({request_id})"))?;
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match rows.len() {
        0 => bail!("retry parent request not found"),
        1 => {}
        count => bail!("retry parent request_id is ambiguous across {count} documents"),
    }
    let row = rows
        .into_iter()
        .next()
        .context("retry parent request absent")?;
    Ok(RetryLineage {
        doc_id: row
            .get("_docID")
            .and_then(Value::as_str)
            .context("retry parent request has no _docID")?
            .to_string(),
        retry_root_request: normalize_optional_string(
            row.get("retry_root_request").and_then(Value::as_str),
        )
        .map(str::to_string),
        created_at: normalize_optional_string(row.get("created_at").and_then(Value::as_str))
            .map(str::to_string),
    })
}

pub(super) fn canonical_request_created_at_after(
    parent_created_at: Option<&str>,
) -> Result<String> {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now = DateTime::parse_from_rfc3339(&now)
        .expect("UTC RFC3339 seconds formatter must parse")
        .with_timezone(&Utc);
    let created_at = match normalize_optional_string(parent_created_at) {
        Some(parent_created_at) => {
            let parent = DateTime::parse_from_rfc3339(parent_created_at)
                .with_context(|| {
                    format!("retry parent request has invalid created_at {parent_created_at:?}")
                })?
                .with_timezone(&Utc);
            now.max(parent + chrono::Duration::seconds(1))
        }
        None => now,
    };
    Ok(created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}
