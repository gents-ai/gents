use gents::config_client::GraphqlEndpoint;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents::{graphql::escape_graphql_string, skills::prompt_slash_skill_selection};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::graphql::GraphqlRequestOptions;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::request_input::RequestInput;
use gents_protocol::row::AgentRequestRow;
use serde::Serialize;
use serde_json::Value;

use crate::{post_graphql, require_non_empty};

#[derive(Debug, thiserror::Error)]
pub(crate) enum RequestHelpersError {
    #[error(transparent)]
    Home(#[from] crate::home_state::HomeStateError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("initialized home {} is required to sign a local request", home.display())]
    HomeNotInitialized { home: PathBuf },
    #[error("local-self request target {target} does not match initialized home node {node_did}")]
    TargetMismatch { target: String, node_did: String },
    #[error("receipt has no physical identity")]
    ReceiptWithoutPhysicalIdentity,
    #[error("receipt has no session")]
    ReceiptWithoutSession,
    #[error("receipt has no node_did")]
    ReceiptWithoutNodeDid,
    #[error("request output omitted physical request identity")]
    OutputOmittedPhysicalIdentity,
    #[error("request output omitted node_did")]
    OutputOmittedNodeDid,
    #[error("request output omitted session")]
    OutputOmittedSession,
    #[error("request output omitted lifecycle state")]
    OutputOmittedLifecycleState,
    #[error("superseded request has no physical output target")]
    SupersededWithoutPhysicalTarget,
    #[error("superseded request targets itself")]
    SupersededTargetsItself,
    #[error("conflicting {noun} ids provided: positional={positional} and {flag_name}={flag}")]
    ConflictingIds {
        noun: String,
        positional: String,
        flag_name: String,
        flag: String,
    },
    #[error("missing {noun} id")]
    MissingId { noun: String },
    #[error("creating output directory {}", path.display())]
    CreateOutputDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encoding JSON output for output file")]
    EncodeOutputFile(#[source] serde_json::Error),
    #[error("writing JSON output file {}", path.display())]
    WriteOutputFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encoding JSON report")]
    EncodeReport(#[source] serde_json::Error),
    #[error("writing JSON report to stdout")]
    WriteReport(#[source] io::Error),
    #[error("writing NDJSON record to stdout")]
    WriteNdjson(#[source] io::Error),
    #[error("duration must not be empty")]
    EmptyDuration,
    #[error("invalid duration number in {raw}")]
    InvalidDurationNumber {
        raw: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("duration overflow")]
    DurationOverflow,
    #[error("unknown duration suffix {suffix:?} (use s, m, h, d)")]
    UnknownDurationSuffix { suffix: String },
    #[error("duration too large")]
    DurationTooLarge(#[source] std::num::TryFromIntError),
    #[error("reading immutable signed submission receipt")]
    ReadReceipt(#[source] gents::config_client::ConfigClientError),
    #[error("request receipt query omitted rows")]
    ReceiptRowsOmitted,
    #[error("ambiguous scoped submission receipt")]
    AmbiguousReceipt,
    #[error("submission receipt crossed exact request scope")]
    ReceiptCrossedScope,
    #[error(transparent)]
    ReceiptSignature(#[from] gents::RequestReceiptError),
    #[error("submission receipt differs from prepared signed request")]
    ReceiptDiffersFromPrepared,
}

pub(crate) fn ensure_local_request_signer(
    home: Option<&Path>,
    target_node_did: &str,
) -> Result<(), RequestHelpersError> {
    if gents::identity::RegisteredIdentity::from_registered_did(target_node_did, None).is_ok() {
        return Ok(());
    }
    let home = crate::resolve_home_dir(home);
    let config = crate::read_init_config(&home)?
        .ok_or_else(|| RequestHelpersError::HomeNotInitialized { home: home.clone() })?;
    if config.node_did.trim() != target_node_did.trim() {
        return Err(RequestHelpersError::TargetMismatch {
            target: target_node_did.to_owned(),
            node_did: config.node_did.clone(),
        });
    }
    crate::load_initialized_home_identity(&home, &config)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct SubmittedRequest {
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) node_did: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) request_doc_id: String,
    pub(crate) requester_did: Option<String>,
    pub(crate) input: Option<RequestInput>,
    pub(crate) created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalToolPresentation {
    pub(crate) arguments: String,
    /// Present only after the invocation reply is durably published.
    pub(crate) result: Option<String>,
    pub(crate) live_output: Option<String>,
}

/// `include_result` must only be requested for a terminal tool lifecycle; a
/// missing or malformed terminal delivery is an error rather than an empty
/// preview.
pub(crate) async fn load_canonical_tool_presentation(
    graphql: &GraphqlEndpoint,
    tool_call_doc_id: &str,
    submitted: &SubmittedRequest,
    include_result: bool,
) -> Result<CanonicalToolPresentation> {
    let access = gents::ConfigAccess::Graphql(graphql.clone());
    let presentation = gents::session::load_tool_call_presentation(
        &access,
        tool_call_doc_id,
        &submitted.node_did,
        &submitted.session_id,
        submitted.requester_did.as_deref(),
    )
    .await
    .context("loading canonical tool presentation")?;
    Ok(CanonicalToolPresentation {
        arguments: presentation.arguments,
        result: include_result.then_some(presentation.result).flatten(),
        live_output: presentation.live_output,
    })
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RequestSubmitOptions {
    pub(crate) caused_by_source_doc_id: Option<String>,
    pub(crate) input: Option<RequestInput>,
    pub(crate) valid_until: Option<DateTime<Utc>>,
    pub(crate) retry_parent_request: Option<String>,
    pub(crate) retry_parent_request_doc_id: Option<String>,
    pub(crate) retry_root_request: Option<String>,
    pub(crate) retry_key: Option<String>,
}

/// An exact `_docID` read carries no `order`: DefraDB plans an ordered read as
/// an index scan and applies `limit` before the `_docID` filter, so later rows
/// in the collection would hide the request.
pub(crate) fn request_terminal_query(request_id: &str, physical: Option<&str>) -> String {
    let (filter, order) = match physical {
        Some(id) => (
            format!("_docID:{{_eq:\"{}\"}}", escape_graphql_string(id)),
            "",
        ),
        None => (
            format!(
                "request_id:{{_eq:\"{}\"}}",
                escape_graphql_string(request_id)
            ),
            "order: { created_at: DESC },",
        ),
    };
    format!(
        r#"{{
            AgentRequest(
                filter: {{ {filter} }},
                {order}
                limit: 2
            ) {{
                _docID node_did requester_did agent_id session_id
                request_id
                lifecycle_state
                failure_reason
                terminal_output
                superseded_by_request
                superseded_by_request_doc_id
                terminalized_at
                interrupt_requested_at
                valid_until
                execution_generation
                execution_lease_secs
                execution_lease_expires_at
            }}
        }}"#,
    )
}

/// The signed admission input remains on the request row and is never
/// rewritten into response-shaped JSON.
pub(crate) async fn observe_canonical_request_output(
    graphql: &GraphqlEndpoint,
    request: &AgentRequestRow,
) -> Result<gents::session::CanonicalRequestOutput> {
    gents::session::observe_request_output(&gents::ConfigAccess::Graphql(graphql.clone()), request)
        .await
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CliRequestMetadata {
    pub(crate) request_doc_id: String,
    pub(crate) request_id: String,
    pub(crate) node_did: String,
    pub(crate) requester_did: Option<String>,
    pub(crate) agent_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) lifecycle_state: RequestLifecycleState,
    pub(crate) failure_reason: Option<String>,
    pub(crate) terminalized_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CliOutputPresentation {
    pub(crate) body_markdown: String,
    pub(crate) reasoning_markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CliOutputObservation {
    Absent,
    Live {
        presentation: CliOutputPresentation,
    },
    Settling {
        presentation: CliOutputPresentation,
    },
    Loading,
    Denied,
    Conflicted,
    Invalid,
    Retracted,
    /// Historical diagnostic only; never serialized as the selected answer.
    RetainedPartial,
    Published {
        header: gents_protocol::output::TranscriptMessage,
        message: gents_protocol::message::Message,
        presentation: CliOutputPresentation,
    },
    TerminalNoMessage,
    TerminalMessage {
        header: gents_protocol::output::TranscriptMessage,
        message: gents_protocol::message::Message,
        presentation: CliOutputPresentation,
    },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RequestOutputEnvelope {
    pub(crate) request: CliRequestMetadata,
    pub(crate) output: CliOutputObservation,
}

fn cli_presentation(value: gents::session::CanonicalPresentation) -> CliOutputPresentation {
    CliOutputPresentation {
        body_markdown: value.body_markdown,
        reasoning_markdown: value.reasoning_markdown,
    }
}

pub(crate) fn request_output_envelope(
    request: &AgentRequestRow,
    output: gents::session::CanonicalRequestOutput,
) -> Result<RequestOutputEnvelope, RequestHelpersError> {
    let request = CliRequestMetadata {
        request_doc_id: request
            .doc_id
            .clone()
            .ok_or(RequestHelpersError::OutputOmittedPhysicalIdentity)?,
        request_id: request.request_id.clone(),
        node_did: request
            .node_did
            .clone()
            .ok_or(RequestHelpersError::OutputOmittedNodeDid)?,
        requester_did: request.requester_did.clone(),
        agent_id: request.agent_id.clone(),
        session_id: request
            .session_id
            .clone()
            .ok_or(RequestHelpersError::OutputOmittedSession)?,
        lifecycle_state: request
            .lifecycle_state
            .ok_or(RequestHelpersError::OutputOmittedLifecycleState)?,
        failure_reason: request.failure_reason.clone(),
        terminalized_at: request.terminalized_at.clone(),
    };
    let output = match output {
        gents::session::CanonicalRequestOutput::Absent => CliOutputObservation::Absent,
        gents::session::CanonicalRequestOutput::Live(value) => CliOutputObservation::Live {
            presentation: cli_presentation(value),
        },
        gents::session::CanonicalRequestOutput::Settling(value) => CliOutputObservation::Settling {
            presentation: cli_presentation(value),
        },
        gents::session::CanonicalRequestOutput::Loading => CliOutputObservation::Loading,
        gents::session::CanonicalRequestOutput::Denied => CliOutputObservation::Denied,
        gents::session::CanonicalRequestOutput::Conflicted => CliOutputObservation::Conflicted,
        gents::session::CanonicalRequestOutput::Invalid => CliOutputObservation::Invalid,
        gents::session::CanonicalRequestOutput::Retracted => CliOutputObservation::Retracted,
        gents::session::CanonicalRequestOutput::RetainedPartial(_) => {
            CliOutputObservation::RetainedPartial
        }
        gents::session::CanonicalRequestOutput::Published {
            header,
            message,
            presentation,
        } => CliOutputObservation::Published {
            header,
            message,
            presentation: cli_presentation(presentation),
        },
        gents::session::CanonicalRequestOutput::TerminalNoMessage => {
            CliOutputObservation::TerminalNoMessage
        }
        gents::session::CanonicalRequestOutput::TerminalMessage {
            header,
            message,
            presentation,
        } => CliOutputObservation::TerminalMessage {
            header,
            message,
            presentation: cli_presentation(presentation),
        },
    };
    Ok(RequestOutputEnvelope { request, output })
}

pub(crate) async fn create_agent_request(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    content: &str,
    session_id: Option<&str>,
    agent_id: Option<&str>,
    options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    let prepared = prepare_agent_request(
        graphql, node_did, content, session_id, agent_id, None, options,
    )
    .await?;
    submit_prepared_agent_request_committed(graphql, &prepared).await
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAgentRequest {
    pub(crate) create: AgentRequestCreate,
}

pub(crate) async fn prepare_agent_request(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    content: &str,
    session_id: Option<&str>,
    agent_id: Option<&str>,
    request_id: Option<String>,
    mut options: RequestSubmitOptions,
) -> Result<PreparedAgentRequest> {
    if let Some(session_id) = session_id.filter(|_| options.retry_parent_request.is_none()) {
        options.input = Some(
            prepare_cli_goal_input(
                &gents::ConfigAccess::Graphql(graphql.clone()),
                node_did,
                session_id,
                options.input.take().unwrap_or_default(),
            )
            .await?,
        );
    }
    prepare_agent_request_with_input(
        graphql, node_did, content, session_id, agent_id, request_id, options,
    )
    .await
}

pub(crate) async fn prepare_agent_request_with_input(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    content: &str,
    session_id: Option<&str>,
    agent_id: Option<&str>,
    request_id: Option<String>,
    options: RequestSubmitOptions,
) -> Result<PreparedAgentRequest> {
    let (request_content, request_input) =
        content_and_input_with_prompt_selected_skill_ids(options.input, content);
    let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let agent_id = resolve_request_agent_id(graphql, node_did, agent_id).await?;
    let session_id = session_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let retry_parent_value = options.retry_parent_request.as_deref().unwrap_or_default();
    let retry_root_value = options
        .retry_root_request
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if retry_parent_value.is_empty() {
                request_id.clone()
            } else {
                retry_parent_value.to_string()
            }
        });
    let admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(node_did);
    let create = gents::build_signed_request(
        gents::RequestSpec {
            trigger_lineage: gents::lifecycle::TriggerLineage {
                source_doc_id: options.caused_by_source_doc_id,
                ..Default::default()
            },
            input: request_input,
            valid_until: options
                .valid_until
                .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            retry_key: options.retry_key,
            retry: Some(gents::RetryLink {
                parent_request_id: (!retry_parent_value.is_empty())
                    .then(|| retry_parent_value.to_string()),
                parent_request_doc_id: options.retry_parent_request_doc_id,
                root_request_id: retry_root_value,
                retry_count: 0,
                max_retries: i64::from(gents::lifecycle::DEFAULT_REQUEST_MAX_RETRIES),
            }),
            ..gents::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents::RequestIdentity {
                    request_id: request_id.clone(),
                    node_did: node_did.to_string(),
                    requester_did: None,
                    agent_id: agent_id.clone(),
                    session_id: session_id.clone(),
                    content: request_content,
                    execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                    created_at: created_at.clone(),
                },
                admission,
            )
        },
        gents::RequestSigner::RegisteredTarget,
    )
    .await?;
    Ok(PreparedAgentRequest { create })
}

pub(crate) async fn prepare_cli_goal_input(
    access: &gents::ConfigAccess,
    node_did: &str,
    session_id: &str,
    input: RequestInput,
) -> Result<RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let verifier = gents::identity::RegisteredIdentity::from_registered_did(node_did, None)?;
    gents::goal::prepare_goal_input(
        access,
        &verifier,
        node_did,
        session_id,
        Some(node_did),
        input,
    )
    .await
}

/// Exact retries use the same immutable submission key and converge to the
/// already-committed pair; conflicting retries fail without mutating either
/// document.
#[expect(
    clippy::too_many_arguments,
    reason = "the request and its Goal are written together from these fields"
)]
pub(crate) async fn create_goal_backed_agent_request(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    content: &str,
    session_id: &str,
    agent_id: Option<&str>,
    objective: &str,
    token_budget: Option<i64>,
    input: RequestInput,
) -> Result<SubmittedRequest> {
    use sha2::{Digest, Sha256};

    let objective = require_non_empty("goal-objective", objective)?.trim();
    anyhow::ensure!(
        token_budget.is_none_or(|budget| budget > 0),
        "goal-token-budget must be positive"
    );
    let digest = Sha256::digest(
        format!("{node_did}\0{session_id}\0{objective}\0{token_budget:?}\0{content}").as_bytes(),
    );
    let request_id = format!(
        "goal-submit-{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let retry_key = format!(
        "goal-submit:{}",
        gents::goal::deterministic_goal_creation_key(node_did, session_id)
    );
    let prepared = prepare_agent_request_with_input(
        graphql,
        node_did,
        content,
        Some(session_id),
        agent_id,
        Some(request_id),
        RequestSubmitOptions {
            retry_key: Some(retry_key.clone()),
            input: Some(input),
            ..Default::default()
        },
    )
    .await?;

    let access = gents::ConfigAccess::Graphql(graphql.clone());
    gents::goal::submit_goal_backed_request(
        &access,
        node_did,
        session_id,
        objective,
        token_budget,
        &prepared.create,
    )
    .await?;
    committed_submitted_request(graphql, &prepared.create).await
}

#[allow(clippy::too_many_arguments)]
#[cfg(unix)]
pub(crate) async fn create_goal_backed_agent_request_local(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    actor: identity::Did,
    graphql: &GraphqlEndpoint,
    node_did: &str,
    objective: &str,
    token_budget: Option<i64>,
    session_id: &str,
    agent_id: &str,
    request_id: String,
    mut options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    options.retry_key = Some(format!("goal-request:{request_id}"));
    let prepared = prepare_agent_request_with_input(
        graphql,
        node_did,
        objective,
        Some(session_id),
        Some(agent_id),
        Some(request_id),
        options,
    )
    .await?;
    gents::goal::submit_goal_backed_request_local(
        node,
        actor,
        node_did,
        session_id,
        objective,
        token_budget,
        &prepared.create,
    )
    .await?;
    committed_submitted_request(graphql, &prepared.create).await
}

pub(crate) async fn submit_prepared_agent_request_committed(
    graphql: &GraphqlEndpoint,
    prepared: &PreparedAgentRequest,
) -> Result<SubmittedRequest> {
    let access = gents::ConfigAccess::Graphql(graphql.clone());
    let mutation = prepared
        .create
        .graphql_mutation()
        .map_err(anyhow::Error::msg)?;
    access
        .write_with_receipt("cli.request.submit", &mutation, || async {
            matching_prepared_receipt(graphql, &prepared.create)
                .await
                .map(|row| row.is_some())
        })
        .await
        .with_context(|| {
            format!(
                "submitting prepared AgentRequest {}",
                prepared.create.request_id
            )
        })?;
    let row = matching_prepared_receipt(graphql, &prepared.create)
        .await?
        .context("committed signed request receipt missing")?;
    Ok(submitted_from_receipt(row)?)
}

async fn committed_submitted_request(
    graphql: &GraphqlEndpoint,
    create: &AgentRequestCreate,
) -> Result<SubmittedRequest> {
    let row = read_submitted_receipt(graphql, create)
        .await?
        .context("committed signed request receipt is missing")?;
    Ok(submitted_from_receipt(row)?)
}

fn submitted_from_receipt(row: AgentRequestRow) -> Result<SubmittedRequest, RequestHelpersError> {
    Ok(SubmittedRequest {
        request_doc_id: row
            .doc_id
            .filter(|id| !id.is_empty())
            .ok_or(RequestHelpersError::ReceiptWithoutPhysicalIdentity)?,
        request_id: row.request_id,
        session_id: row
            .session_id
            .ok_or(RequestHelpersError::ReceiptWithoutSession)?,
        node_did: row
            .node_did
            .ok_or(RequestHelpersError::ReceiptWithoutNodeDid)?,
        requester_did: row.requester_did,
        agent_id: row.agent_id,
        input: row.input,
        created_at: row.created_at,
    })
}

pub(crate) async fn matching_prepared_receipt(
    graphql: &GraphqlEndpoint,
    create: &AgentRequestCreate,
) -> Result<Option<AgentRequestRow>, RequestHelpersError> {
    let Some(row) = read_submitted_receipt(graphql, create).await? else {
        return Ok(None);
    };
    let signature = bs58::encode(&create.admission.signature).into_string();
    if !(row.admission_signer_did.as_deref() == Some(create.admission.signer_did.as_str())
        && row.admission_signature.as_deref() == Some(signature.as_str()))
    {
        return Err(RequestHelpersError::ReceiptDiffersFromPrepared);
    }
    Ok(Some(row))
}

async fn read_submitted_receipt(
    graphql: &GraphqlEndpoint,
    create: &AgentRequestCreate,
) -> Result<Option<AgentRequestRow>, RequestHelpersError> {
    let scope = gents::session::session_scope_filter(
        &create.node_did,
        &create.session_id,
        (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()),
    );
    let logical = escape_graphql_string(&create.request_id);
    let response = gents::config_client::query_graphql_with_options(
        graphql,
        &format!(
            "{{AgentRequest(filter:{{{scope},request_id:{{_eq:\"{logical}\"}}}}){{{}}}}}",
            gents::SIGNED_REQUEST_FIELDS
        ),
        GraphqlRequestOptions {
            timeout: Duration::from_secs(30),
            max_attempts: 1,
            retry_backoff: Duration::ZERO,
        },
    )
    .await
    .map_err(RequestHelpersError::ReadReceipt)?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .ok_or(RequestHelpersError::ReceiptRowsOmitted)?;
    if rows.len() > 1 {
        return Err(RequestHelpersError::AmbiguousReceipt);
    }
    let Some(value) = rows.first() else {
        return Ok(None);
    };
    let row: AgentRequestRow = serde_json::from_value(value.clone())?;
    if !(row.request_id == create.request_id
        && row.node_did.as_deref() == Some(create.node_did.as_str())
        && row.session_id.as_deref() == Some(create.session_id.as_str())
        && row.requester_did.as_deref()
            == (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()))
    {
        return Err(RequestHelpersError::ReceiptCrossedScope);
    }
    gents::verify_request_receipt_signature(&row)?;
    Ok(Some(row))
}

async fn resolve_request_agent_id(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    requested: Option<&str>,
) -> Result<String> {
    let escaped_node_did = gents::graphql::escape_graphql_string(node_did);
    let response = post_graphql(
        graphql,
        &format!(
            r#"{{
                Node(
                    filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                    limit: 2
                ) {{ node_did default_agent_id enabled }}
                Agent(
                    filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }}
                ) {{ agent_id node_did enabled }}
            }}"#,
        ),
    )
    .await
    .context("loading authoritative request agent")?;
    let nodes = response
        .pointer("/data/Node")
        .and_then(Value::as_array)
        .context("Node query returned no row array")?;
    anyhow::ensure!(
        nodes.len() == 1,
        "request target node must resolve to exactly one row"
    );
    let principal = &nodes[0];
    anyhow::ensure!(
        principal.get("enabled").and_then(Value::as_bool) == Some(true),
        "request target node is disabled"
    );
    let agent_id = match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(agent_id) => agent_id.to_string(),
        None => principal
            .get("default_agent_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .context("request target node has no canonical default agent")?,
    };
    let agents = response
        .pointer("/data/Agent")
        .and_then(Value::as_array)
        .context("Agent query returned no row array")?;
    let matching = agents
        .iter()
        .filter(|row| row.get("agent_id").and_then(Value::as_str) == Some(&agent_id))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matching.len() == 1,
        "request agent must resolve to exactly one row owned by the target node"
    );
    anyhow::ensure!(
        matching[0].get("enabled").and_then(Value::as_bool) == Some(true),
        "request agent is disabled"
    );
    Ok(agent_id)
}

pub(crate) fn content_and_input_with_prompt_selected_skill_ids(
    input: Option<RequestInput>,
    content: &str,
) -> (String, RequestInput) {
    let selection = prompt_slash_skill_selection(content);
    let mut input = input.unwrap_or_default();
    for id in selection.selected_skill_ids {
        if !input.selected_skill_ids.contains(&id) {
            input.selected_skill_ids.push(id);
        }
    }
    (selection.prompt, input)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WaitProgressMarker {
    request_lifecycle_state: Option<RequestLifecycleState>,
    request_failure_reason: Option<String>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    request_execution_lease_expires_at: Option<String>,
    request_terminal_output: Option<String>,
    request_terminalized_at: Option<String>,
    canonical_output: Option<WaitOutputProgress>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WaitOutputProgress {
    kind: &'static str,
    body_markdown: String,
    reasoning_markdown: Option<String>,
}

fn wait_output_progress(
    output: &gents::session::CanonicalRequestOutput,
) -> Option<WaitOutputProgress> {
    let (kind, presentation) = match output {
        gents::session::CanonicalRequestOutput::Live(value) => ("live", value),
        gents::session::CanonicalRequestOutput::Settling(value) => ("settling", value),
        gents::session::CanonicalRequestOutput::Published { presentation, .. } => {
            ("published", presentation)
        }
        _ => return None,
    };
    Some(WaitOutputProgress {
        kind,
        body_markdown: presentation.body_markdown.clone(),
        reasoning_markdown: presentation.reasoning_markdown.clone(),
    })
}

fn wait_progress_marker(
    request_row: Option<&AgentRequestRow>,
    output: Option<&gents::session::CanonicalRequestOutput>,
) -> WaitProgressMarker {
    WaitProgressMarker {
        request_lifecycle_state: request_row.and_then(|row| row.lifecycle_state),
        request_failure_reason: request_row.and_then(|row| row.failure_reason.clone()),
        request_interrupt_requested_at: request_row
            .and_then(|row| row.interrupt_requested_at.clone()),
        request_valid_until: request_row.and_then(|row| row.valid_until.clone()),
        request_execution_lease_expires_at: request_row
            .and_then(|row| row.execution_lease_expires_at.clone()),
        request_terminal_output: request_row
            .and_then(|row| row.terminal_output.as_ref())
            .and_then(|selection| serde_json::to_string(selection).ok()),
        request_terminalized_at: request_row.and_then(|row| row.terminalized_at.clone()),
        canonical_output: output.and_then(wait_output_progress),
    }
}

fn successor_output_target(
    row: &AgentRequestRow,
) -> Result<Option<(String, String)>, RequestHelpersError> {
    let Some(logical) =
        gents::lifecycle::folded_into(row).or_else(|| gents::lifecycle::replaced_by(row))
    else {
        return Ok(None);
    };
    let physical = row
        .superseded_by_request_doc_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or(RequestHelpersError::SupersededWithoutPhysicalTarget)?;
    if row.doc_id.as_deref() == Some(physical) {
        return Err(RequestHelpersError::SupersededTargetsItself);
    }
    Ok(Some((logical.to_owned(), physical.to_owned())))
}

pub(crate) async fn wait_for_terminal_response(
    graphql: &GraphqlEndpoint,
    request_id: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<RequestOutputEnvelope> {
    wait_for_terminal_response_with_progress(
        graphql,
        request_id,
        timeout_secs,
        poll_secs,
        || async { Ok(false) },
    )
    .await
}

pub(crate) async fn wait_for_terminal_response_with_progress<F, Fut>(
    graphql: &GraphqlEndpoint,
    request_id: &str,
    timeout_secs: u64,
    poll_secs: u64,
    mut progress: F,
) -> Result<RequestOutputEnvelope>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut last_progress_marker: Option<WaitProgressMarker> = None;

    let mut pinned: Option<AgentRequestRow> = None;
    let mut current_request_id = request_id.to_owned();
    let mut target_physical: Option<String> = None;
    let mut followed = std::collections::BTreeSet::new();
    let mut original_scope: Option<AgentRequestRow> = None;
    loop {
        if progress().await? {
            last_progress_at = tokio::time::Instant::now();
        }
        let request_row = {
            let query = request_terminal_query(
                &current_request_id,
                pinned
                    .as_ref()
                    .and_then(|row| row.doc_id.as_deref())
                    .or(target_physical.as_deref()),
            );
            let response = post_graphql(graphql, &query).await?;
            let rows = response
                .pointer("/data/AgentRequest")
                .and_then(Value::as_array)
                .context("terminal request query omitted rows")?;
            anyhow::ensure!(
                rows.len() <= 1,
                "ambiguous request ID while selecting terminal request"
            );
            rows.first()
                .as_ref()
                .map(|row| {
                    serde_json::from_value::<AgentRequestRow>((*row).clone())
                        .context("decoding terminal-wait AgentRequest row")
                })
                .transpose()?
        };
        if let Some(request) = request_row.as_ref() {
            anyhow::ensure!(
                request.doc_id.is_some(),
                "terminal request has no physical identity"
            );
            anyhow::ensure!(
                request.request_id == current_request_id,
                "successor output target changed logical identity"
            );
            if let Some(expected) = target_physical.as_deref() {
                anyhow::ensure!(
                    request.doc_id.as_deref() == Some(expected),
                    "successor output changed physical target"
                );
            }
            if let Some(original) = original_scope.as_ref() {
                anyhow::ensure!(
                    request.node_did == original.node_did
                        && request.requester_did == original.requester_did
                        && request.session_id == original.session_id
                        && request.agent_id == original.agent_id,
                    "successor output target crossed request scope"
                );
            } else {
                original_scope = Some(request.clone());
            }
            if let Some(original) = pinned.as_ref() {
                anyhow::ensure!(
                    request.node_did == original.node_did
                        && request.requester_did == original.requester_did
                        && request.session_id == original.session_id
                        && request.request_id == original.request_id,
                    "terminal request scope changed"
                );
            } else {
                pinned = Some(request.clone());
            }
        }
        if let Some(request) = request_row.as_ref() {
            if let Some((logical, physical)) = successor_output_target(request)? {
                followed.insert(request.doc_id.clone().unwrap());
                anyhow::ensure!(
                    !followed.contains(&physical),
                    "cycle in successor output targets"
                );
                current_request_id = logical;
                target_physical = Some(physical);
                pinned = None;
                continue;
            }
        }
        let mut observed_output = match request_row.as_ref() {
            Some(request) => Some(observe_canonical_request_output(graphql, request).await?),
            None => None,
        };
        match observed_output.as_ref() {
            Some(gents::session::CanonicalRequestOutput::Denied) => {
                anyhow::bail!("canonical output for request {request_id} is denied")
            }
            Some(gents::session::CanonicalRequestOutput::Conflicted) => {
                anyhow::bail!("canonical output for request {request_id} is conflicted")
            }
            Some(gents::session::CanonicalRequestOutput::Invalid) => {
                anyhow::bail!("canonical output for request {request_id} is invalid")
            }
            _ => {}
        }
        let marker = wait_progress_marker(request_row.as_ref(), observed_output.as_ref());
        if last_progress_marker.as_ref() != Some(&marker) {
            last_progress_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }

        let lifecycle_state = request_row.as_ref().and_then(|row| row.lifecycle_state);
        let terminal_by_request = lifecycle_state.is_some_and(RequestLifecycleState::is_terminal);
        if terminal_by_request {
            let request = request_row
                .as_ref()
                .context("terminal request disappeared after exact selection")?;
            let output = observed_output
                .take()
                .context("terminal request output observation is missing")?;
            match output {
                gents::session::CanonicalRequestOutput::Loading => {
                    if last_progress_at.elapsed() >= idle_timeout {
                        anyhow::bail!(
                            "timed out waiting for materialized AgentMessage {request_id} after {timeout_secs}s of inactivity\n{}",
                            request_diagnostic_hint(request_id)
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
                gents::session::CanonicalRequestOutput::Denied => {
                    anyhow::bail!("canonical terminal output for request {request_id} is denied")
                }
                gents::session::CanonicalRequestOutput::Conflicted => anyhow::bail!(
                    "canonical terminal output for request {request_id} is conflicted"
                ),
                gents::session::CanonicalRequestOutput::Invalid => {
                    anyhow::bail!("canonical terminal output for request {request_id} is invalid")
                }
                gents::session::CanonicalRequestOutput::Retracted => anyhow::bail!(
                    "canonical terminal output for request {request_id} is retracted"
                ),
                gents::session::CanonicalRequestOutput::RetainedPartial(_) => anyhow::bail!(
                    "canonical terminal output for request {request_id} resolved only to retained partial diagnostics"
                ),
                gents::session::CanonicalRequestOutput::TerminalNoMessage
                | gents::session::CanonicalRequestOutput::TerminalMessage { .. } => {
                    return Ok(request_output_envelope(request, output)?)
                }
                gents::session::CanonicalRequestOutput::Absent
                | gents::session::CanonicalRequestOutput::Live(_)
                | gents::session::CanonicalRequestOutput::Settling(_)
                | gents::session::CanonicalRequestOutput::Published { .. } => anyhow::bail!(
                    "terminal request {request_id} produced a nonterminal canonical output observation"
                ),
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentRequest {request_id} after {timeout_secs}s of inactivity\n{}",
                request_diagnostic_hint(request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

pub(crate) fn request_diagnostic_hint(request_id: &str) -> String {
    format!(
        "Next:\n  1. Run `gents request show {request_id}`\n  2. Run `gents response show {request_id}`\n  3. Inspect the runtime with `gents status`"
    )
}

pub(crate) fn resolve_dual_id(
    noun: &str,
    flag_name: &str,
    positional: Option<&str>,
    flag: Option<&str>,
) -> Result<String, RequestHelpersError> {
    let positional = positional.map(str::trim).filter(|value| !value.is_empty());
    let flag = flag.map(str::trim).filter(|value| !value.is_empty());
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => {
            Err(RequestHelpersError::ConflictingIds {
                noun: noun.to_owned(),
                positional: positional.to_owned(),
                flag_name: flag_name.to_owned(),
                flag: flag.to_owned(),
            })
        }
        (Some(request_id), _) | (_, Some(request_id)) => Ok(request_id.to_string()),
        (None, None) => Err(RequestHelpersError::MissingId {
            noun: noun.to_owned(),
        }),
    }
}

pub(crate) fn resolve_request_id(
    positional: Option<&str>,
    flag: Option<&str>,
) -> Result<String, RequestHelpersError> {
    resolve_dual_id("request", "--request-id", positional, flag)
}

pub(crate) fn resolve_request_content(
    content: Option<&str>,
    content_file: Option<&Path>,
) -> Result<String> {
    match (content, content_file) {
        (Some(_), Some(path)) => anyhow::bail!(
            "provide either --content or --content-file, not both ({})",
            path.display()
        ),
        (Some(content), None) => Ok(require_non_empty("content", content)?.to_string()),
        (None, Some(path)) => {
            let content = fs::read_to_string(path)
                .with_context(|| format!("reading request content from {}", path.display()))?;
            Ok(require_non_empty("content-file", &content)?.to_string())
        }
        (None, None) => {
            anyhow::bail!("request content is required; pass --content or --content-file")
        }
    }
}

pub(crate) fn write_json_output_file(
    path: &Path,
    value: &Value,
) -> Result<(), RequestHelpersError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            RequestHelpersError::CreateOutputDirectory {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    let contents =
        serde_json::to_vec_pretty(value).map_err(RequestHelpersError::EncodeOutputFile)?;
    fs::write(path, contents).map_err(|source| RequestHelpersError::WriteOutputFile {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

tokio::task_local! {
    /// Set while a command runs for an embedding caller (the desktop): its
    /// report is kept here instead of printed.
    static CAPTURED_REPORT: std::cell::RefCell<Option<Value>>;
}

pub(crate) fn print_json<T>(value: &T) -> Result<(), RequestHelpersError>
where
    T: serde::Serialize + ?Sized,
{
    if CAPTURED_REPORT.try_with(|_| ()).is_ok() {
        let value = serde_json::to_value(value).map_err(RequestHelpersError::EncodeReport)?;
        CAPTURED_REPORT.with(|report| *report.borrow_mut() = Some(value));
        return Ok(());
    }
    // `println!` panics on a closed stdout; a report that cannot be written is
    // an ordinary error so long-running callers can still shut down in order.
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string_pretty(value)?)
        .and_then(|()| stdout.flush())
        .map_err(RequestHelpersError::WriteReport)
}

pub(crate) fn print_ndjson(value: &Value) -> Result<(), RequestHelpersError> {
    if CAPTURED_REPORT
        .try_with(|report| *report.borrow_mut() = Some(value.clone()))
        .is_ok()
    {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string(value)?)
        .and_then(|()| stdout.flush())
        .map_err(RequestHelpersError::WriteNdjson)
}

pub(crate) async fn capture_report<F>(command: F) -> Result<Value>
where
    F: std::future::Future<Output = Result<()>>,
{
    CAPTURED_REPORT
        .scope(std::cell::RefCell::new(None), async move {
            command.await?;
            CAPTURED_REPORT
                .with(|report| report.borrow_mut().take())
                .ok_or_else(|| anyhow::anyhow!("the command produced no report"))
        })
        .await
}

pub(crate) fn parse_duration_suffix(raw: &str) -> Result<Duration, RequestHelpersError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(RequestHelpersError::EmptyDuration);
    }
    let split = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let (num_part, suffix) = s.split_at(split);
    let n: u64 =
        num_part
            .parse::<u64>()
            .map_err(|source| RequestHelpersError::InvalidDurationNumber {
                raw: raw.to_owned(),
                source,
            })?;
    let secs = match suffix {
        "" | "s" => n,
        "m" => n
            .checked_mul(60)
            .ok_or(RequestHelpersError::DurationOverflow)?,
        "h" => n
            .checked_mul(3600)
            .ok_or(RequestHelpersError::DurationOverflow)?,
        "d" => n
            .checked_mul(86400)
            .ok_or(RequestHelpersError::DurationOverflow)?,
        other => {
            return Err(RequestHelpersError::UnknownDurationSuffix {
                suffix: other.to_owned(),
            })
        }
    };
    Ok(Duration::from_secs(secs))
}

pub(crate) fn parse_valid_until_flag(
    raw: Option<&str>,
) -> Result<Option<DateTime<Utc>>, RequestHelpersError> {
    match raw.map(str::trim) {
        None => Ok(Some(Utc::now() + chrono::Duration::minutes(5))),
        Some("") | Some("none") | Some("0") => Ok(None),
        Some(value) => {
            let dur = parse_duration_suffix(value)?;
            let secs =
                i64::try_from(dur.as_secs()).map_err(RequestHelpersError::DurationTooLarge)?;
            Ok(Some(Utc::now() + chrono::Duration::seconds(secs)))
        }
    }
}

pub(crate) async fn fetch_request_view(
    graphql: &GraphqlEndpoint,
    request_id: &str,
) -> Result<AgentRequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 2
            ) {{
                _docID
                request_id
                node_did
                agent_id
                content
                lifecycle_state
                failure_reason
                terminal_output
                terminalized_at
                execution_generation
                execution_lease_secs
                execution_lease_expires_at
                retry_root_request
                requester_did
                session_id
                input
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.len() != 1 {
        anyhow::bail!(
            "request_id {request_id} is ambiguous or absent across {} AgentRequest documents",
            rows.len()
        );
    }
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    serde_json::from_value(row).with_context(|| format!("decoding AgentRequest {request_id}"))
}

#[cfg(test)]
pub(crate) mod goal_input_tests;

#[cfg(test)]
mod tests {
    use super::{submit_prepared_agent_request_committed, PreparedAgentRequest};
    use super::{successor_output_target, CliOutputObservation};
    use axum::{
        body::{Body, Bytes},
        extract::State,
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use futures_util::stream;
    use gents::config_client::GraphqlEndpoint;
    use gents_protocol::row::AgentRequestRow;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    /// The executable SessionCompositionCases.retractedPhase continues through
    /// delayedPhase before terminalization; a source retraction is not a request terminal.
    #[tokio::test]
    async fn terminal_wait_survives_active_retraction_and_preserves_later_failure(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter, SourceClose};
        use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let snapshot: Value = gents_lean_contract::load_contract_snapshot()?;
        let modeled = snapshot["canonical_output_projection_cases"]
            .as_array()
            .expect("generated canonical projection cases")
            .iter()
            .find(|case| case["name"] == "retracted_attempt_is_not_rendered")
            .expect("generated nonterminal retraction case");
        assert_eq!(modeled["input"]["request_terminal"], false);
        assert_eq!(modeled["expected"]["kind"], "retracted");
        let modeled_close: SourceClose =
            serde_json::from_value(modeled["input"]["records"][0]["close"].clone())?;
        let close = OutputSegment {
            node_did: "did:test:owner".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "physical".into(),
            source: OutputSource::ProviderTurn {
                scope: CaptureScope {
                    kind: CaptureScopeKind::Inference,
                    seq: 1,
                },
                turn_index: 0,
                attempt: 0,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: None,
            runs: vec![],
            payload: String::new(),
            close: Some(modeled_close),
            created_at: "2026-09-01T00:00:00Z".into(),
        };
        let mut close = serde_json::to_value(close)?;
        close["_docID"] = json!("retracted-attempt");
        let output_reads = Arc::new(AtomicUsize::new(0));
        let request_reads = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route("/graphql", post({
            let output_reads = output_reads.clone();
            let request_reads = request_reads.clone();
            move |Json(body): Json<Value>| {
                let output_reads = output_reads.clone();
                let request_reads = request_reads.clone();
                let close = close.clone();
                async move {
                    if body["query"].as_str().unwrap().contains("AgentOutputSegment") {
                        output_reads.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"data": {"AgentOutputSegment": [close], "AgentMessage": []}}))
                    } else {
                        request_reads.fetch_add(1, Ordering::SeqCst);
                        let terminal = output_reads.load(Ordering::SeqCst) > 0;
                        Json(json!({"data": {"AgentRequest": [{
                            "_docID": "physical", "request_id": "request",
                            "node_did": "did:test:owner", "requester_did": null,
                            "agent_id": "engineer", "session_id": "session",
                            "lifecycle_state": if terminal { "failed" } else { "processing" },
                            "execution_generation": "generation",
                            "execution_lease_secs": 30,
                            "execution_lease_expires_at": "2026-09-01T00:00:30Z",
                            "failure_reason": if terminal { Some("provider returned an empty response") } else { None },
                            "terminal_output": if terminal { Some(json!({"kind": "no_message"})) } else { None },
                            "terminalized_at": if terminal { Some("2026-09-01T00:00:01Z") } else { None }
                        }]}}))
                    }
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = gents::config_client::GraphqlEndpoint::anonymous(format!(
            "http://{}/graphql",
            listener.local_addr()?
        ));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let result = super::wait_for_terminal_response(&endpoint, "request", 5, 0).await;
        server.abort();
        let result = result?;
        assert_eq!(output_reads.load(Ordering::SeqCst), 1);
        assert_eq!(request_reads.load(Ordering::SeqCst), 2);
        assert_eq!(
            result.request.lifecycle_state,
            gents_protocol::client_protocol::RequestLifecycleState::Failed
        );
        assert_eq!(
            result.request.failure_reason.as_deref(),
            Some("provider returned an empty response")
        );
        assert!(matches!(
            result.output,
            super::CliOutputObservation::TerminalNoMessage
        ));
        Ok(())
    }

    #[derive(Clone, Default)]
    struct AmbiguousSubmitState {
        durable_ids: Arc<Mutex<BTreeSet<String>>>,
        receipt: Arc<Mutex<Value>>,
        mutation_count: Arc<std::sync::atomic::AtomicUsize>,
        lose_transport_response: Arc<std::sync::atomic::AtomicBool>,
    }

    async fn ambiguous_submit_endpoint(
        State(state): State<AmbiguousSubmitState>,
        Json(body): Json<Value>,
    ) -> Response {
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if query.contains("create_AgentRequest") {
            state
                .mutation_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            state
                .durable_ids
                .lock()
                .expect("durable ids")
                .insert("stable-request-id".to_string());
            if state
                .lose_transport_response
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                // The durable write happened, then the response body failed.
                // reqwest retains this decode/transport error in its source
                // chain, which recovery must classify before querying
                // the stable request id.
                let body = Body::from_stream(stream::once(async {
                    Err::<Bytes, std::io::Error>(std::io::Error::other(
                        "connection closed before message completed",
                    ))
                }));
                return Response::new(body);
            }
            // The mutation committed, but its response was lost/replaced by a
            // transient error at the transport boundary.
            return Json(json!({"errors": [{"message": "transaction conflict. please retry"}]}))
                .into_response();
        }
        let rows: Vec<Value> = state
            .durable_ids
            .lock()
            .expect("durable ids")
            .iter()
            .map(|_| state.receipt.lock().unwrap().clone())
            .collect();
        Json(json!({"data": {"AgentRequest": rows}})).into_response()
    }

    async fn stable_test_prepared_request() -> (PreparedAgentRequest, Value) {
        use gents::NodeIdentity;
        let dir = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
        let did = identity.did().to_string();
        let create = gents::build_signed_request(
            gents::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents::RequestIdentity {
                    request_id: "stable-request-id".into(),
                    node_did: did.clone(),
                    requester_did: None,
                    agent_id: "agent".into(),
                    session_id: "session".into(),
                    content: "hello".into(),
                    execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                    created_at: "2026-09-01T00:00:00Z".into(),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
            ),
            gents::RequestSigner::Identity(&identity),
        )
        .await
        .unwrap();
        let receipt = json!({
            "_docID":"physical-receipt", "request_id":create.request_id, "purpose":"normal", "node_did":create.node_did,
            "requester_did":create.requester_did, "agent_id":create.agent_id, "session_id":create.session_id,
            "content":create.content, "input":create.input, "execution_origin":create.execution_origin,
            "created_at":create.created_at, "retry_parent_request":create.retry_parent_request,
            "retry_root_request":create.retry_root_request, "retry_count":create.retry_count,
            "max_retries":create.max_retries, "request_hop":create.request_hop,
            "admission_kind":"local-self", "admission_signer_did":create.admission.signer_did,
            "admission_signature":bs58::encode(&create.admission.signature).into_string()
        });
        gents::verify_request_receipt_signature(&serde_json::from_value(receipt.clone()).unwrap())
            .unwrap();
        (PreparedAgentRequest { create }, receipt)
    }

    #[tokio::test]
    async fn transient_submission_recovers_committed_identity_without_reposting() {
        let state = AmbiguousSubmitState::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test endpoint");
        let address = listener.local_addr().expect("test endpoint address");
        let router = Router::new()
            .route("/", post(ambiguous_submit_endpoint))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let (prepared, receipt) = stable_test_prepared_request().await;
        *state.receipt.lock().unwrap() = receipt;
        let submitted = submit_prepared_agent_request_committed(
            &gents::config_client::GraphqlEndpoint::anonymous(format!("http://{address}/")),
            &prepared,
        )
        .await
        .expect("recover committed request");
        assert_eq!(submitted.request_id, "stable-request-id");
        assert_eq!(
            state
                .mutation_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a committed mutation must not be posted again"
        );
        assert_eq!(state.durable_ids.lock().expect("durable ids").len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn transport_response_loss_recovers_committed_identity_without_reposting() {
        let state = AmbiguousSubmitState {
            lose_transport_response: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ..Default::default()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test endpoint");
        let address = listener.local_addr().expect("test endpoint address");
        let router = Router::new()
            .route("/", post(ambiguous_submit_endpoint))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let (prepared, receipt) = stable_test_prepared_request().await;
        *state.receipt.lock().unwrap() = receipt;
        let submitted = submit_prepared_agent_request_committed(
            &gents::config_client::GraphqlEndpoint::anonymous(format!("http://{address}/")),
            &prepared,
        )
        .await
        .expect("recover committed request after transport response loss");
        assert_eq!(submitted.request_id, "stable-request-id");
        assert_eq!(
            state
                .mutation_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a committed mutation must not be posted again after response loss"
        );
        assert_eq!(state.durable_ids.lock().expect("durable ids").len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn submission_receipt_rejects_tampered_signed_content() {
        let (prepared, mut receipt) = stable_test_prepared_request().await;
        receipt["content"] = json!("substituted content");
        let state = AmbiguousSubmitState::default();
        state
            .durable_ids
            .lock()
            .unwrap()
            .insert(prepared.create.request_id.clone());
        *state.receipt.lock().unwrap() = receipt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = gents::config_client::GraphqlEndpoint::anonymous(format!(
            "http://{}/",
            listener.local_addr().unwrap()
        ));
        let router = Router::new()
            .route("/", post(ambiguous_submit_endpoint))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        assert!(
            super::matching_prepared_receipt(&endpoint, &prepared.create)
                .await
                .is_err()
        );
        assert_eq!(
            state
                .mutation_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        server.abort();
    }

    #[tokio::test]
    async fn terminal_wait_follows_only_exact_same_scope_successor_output_owner(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for (cross_scope, reason) in [
            (false, gents::lifecycle::FOLDED_REASON),
            (true, gents::lifecycle::FOLDED_REASON),
            (false, gents::lifecycle::REPLACED_REASON),
        ] {
            let queries = Arc::new(Mutex::new(Vec::<String>::new()));
            let target_polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let app = Router::new().route("/graphql", post({
                let queries = queries.clone();
                let target_polls = target_polls.clone();
                move |Json(body): Json<Value>| {
                    let queries = queries.clone();
                    let target_polls = target_polls.clone();
                    async move {
                        let query = body["query"].as_str().unwrap().to_owned();
                        queries.lock().unwrap().push(query.clone());
                        if query.contains("AgentOutputSegment") {
                            return Json(json!({"data":{"AgentOutputSegment":[],"AgentMessage":[]}}));
                        }
                        let target = query.contains("active-physical");
                        let output_visible = !target || target_polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
                        Json(json!({"data":{"AgentRequest":[{
                            "_docID": if target {"active-physical"} else {"input-physical"},
                            "request_id": if target {"active"} else {"input"},
                            "node_did":"did:test:owner", "requester_did":"did:test:author",
                            "session_id": if target && cross_scope {"other-session"} else {"same-session"},
                            "agent_id":"engineer", "lifecycle_state": if target {"completed"} else {"superseded"},
                            "failure_reason": if target {Value::Null} else {json!(reason)},
                            "superseded_by_request": if target {Value::Null} else {json!("active")},
                            "superseded_by_request_doc_id": if target {Value::Null} else {json!("active-physical")},
                            "terminal_output":if output_visible {json!({"kind":"no_message"})} else {Value::Null},
                            "terminalized_at":"2026-10-10T00:00:00Z"
                        }]}}))
                    }
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let endpoint =
                GraphqlEndpoint::anonymous(format!("http://{}/graphql", listener.local_addr()?));
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let progress_calls = std::sync::atomic::AtomicUsize::new(0);
            let result = super::wait_for_terminal_response_with_progress(
                &endpoint,
                "input",
                5,
                0,
                || async {
                    progress_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(true)
                },
            )
            .await;
            assert!(progress_calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);

            server.abort();
            if cross_scope {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("crossed request scope"));
            } else {
                let result = result?;
                assert!(
                    target_polls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
                    "terminal output lag must be retried"
                );
                assert_eq!(result.request.request_id, "active");
                assert_eq!(result.request.request_doc_id, "active-physical");
                assert!(matches!(
                    result.output,
                    CliOutputObservation::TerminalNoMessage
                ));
            }
            let queries = queries.lock().unwrap();
            assert!(
                queries
                    .iter()
                    .any(|query| query.contains("_docID:{_eq:\"active-physical\"}")),
                "{queries:?}"
            );
            assert!(
                !queries
                    .iter()
                    .any(|query| query.contains("request_id:{_eq:\"active\"}")),
                "target must be selected physically"
            );
        }
        Ok(())
    }

    #[test]
    fn successor_output_target_rejects_missing_physical_identity_and_self_cycles() {
        let mut row: AgentRequestRow = serde_json::from_value(json!({
            "_docID":"input", "request_id":"input", "lifecycle_state":"superseded",
            "failure_reason":gents::lifecycle::FOLDED_REASON, "superseded_by_request":"active"
        }))
        .unwrap();
        assert!(successor_output_target(&row).is_err());
        row.superseded_by_request_doc_id = Some("input".into());
        assert!(successor_output_target(&row).is_err());
        row.failure_reason = Some(gents::lifecycle::REPLACED_REASON.into());
        row.superseded_by_request_doc_id = Some("replacement".into());
        assert_eq!(
            successor_output_target(&row).unwrap(),
            Some(("active".into(), "replacement".into())),
            "an edited input is answered by its replacement"
        );
        row.failure_reason = Some("other supersession".into());
        assert!(successor_output_target(&row).unwrap().is_none());
    }
}
