use std::fs;
use std::io::{self, Write};
use std::path::Path;
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

pub(crate) fn ensure_local_request_signer(
    home: Option<&Path>,
    target_agent_did: &str,
) -> Result<()> {
    if gents::identity::RegisteredIdentity::from_registered_did(target_agent_did, None).is_ok() {
        return Ok(());
    }
    let home = crate::resolve_home_dir(home);
    let config = crate::read_init_config(&home)?.with_context(|| {
        format!(
            "initialized home {} is required to sign a local request",
            home.display()
        )
    })?;
    anyhow::ensure!(
        config.agent_did.trim() == target_agent_did.trim(),
        "local-self request target {} does not match initialized home principal {}",
        target_agent_did,
        config.agent_did
    );
    crate::load_initialized_home_identity(&home, &config)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct SubmittedRequest {
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
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

/// Resolve CLI tool presentation through the physical canonical owners.
/// `include_result` must only be requested for a terminal tool lifecycle; a
/// missing or malformed terminal delivery is an error rather than an empty
/// preview.
pub(crate) async fn load_canonical_tool_presentation(
    graphql: &str,
    tool_call_doc_id: &str,
    submitted: &SubmittedRequest,
    include_result: bool,
) -> Result<CanonicalToolPresentation> {
    let access = gents::ConfigAccess::Graphql(graphql.to_owned());
    let presentation = gents::session::load_tool_call_presentation(
        &access,
        tool_call_doc_id,
        &submitted.agent_did,
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

pub(crate) fn request_terminal_query(request_id: &str, physical: Option<&str>) -> String {
    let filter = match physical {
        Some(id) => format!("_docID:{{_eq:\"{}\"}}", escape_graphql_string(id)),
        None => format!(
            "request_id:{{_eq:\"{}\"}}",
            escape_graphql_string(request_id)
        ),
    };
    format!(
        r#"{{
            AgentRequest(
                filter: {{ {filter} }},
                order: {{ created_at: DESC }},
                limit: 2
            ) {{
                _docID agent_did requester_did behavior_id session_id
                request_id
                lifecycle_state
                failure_reason
                terminal_output
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

/// Typed canonical request-output observation shared by CLI transports. The
/// signed admission input remains on the request row and is never rewritten
/// into response-shaped JSON.
pub(crate) async fn observe_canonical_request_output(
    graphql: &str,
    request: &AgentRequestRow,
) -> Result<gents::session::CanonicalRequestOutput> {
    gents::session::observe_request_output(
        &gents::ConfigAccess::Graphql(graphql.to_owned()),
        request,
    )
    .await
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CliRequestMetadata {
    pub(crate) request_doc_id: String,
    pub(crate) request_id: String,
    pub(crate) agent_did: String,
    pub(crate) requester_did: Option<String>,
    pub(crate) behavior_id: Option<String>,
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
) -> Result<RequestOutputEnvelope> {
    let request = CliRequestMetadata {
        request_doc_id: request
            .doc_id
            .clone()
            .context("request output omitted physical request identity")?,
        request_id: request.request_id.clone(),
        agent_did: request
            .agent_did
            .clone()
            .context("request output omitted principal")?,
        requester_did: request.requester_did.clone(),
        behavior_id: request.behavior_id.clone(),
        session_id: request
            .session_id
            .clone()
            .context("request output omitted session")?,
        lifecycle_state: request
            .lifecycle_state
            .context("request output omitted lifecycle state")?,
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
    graphql: &str,
    agent_did: &str,
    content: &str,
    session_id: Option<&str>,
    behavior_id: Option<&str>,
    options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    let prepared = prepare_agent_request(
        graphql,
        agent_did,
        content,
        session_id,
        behavior_id,
        None,
        options,
    )
    .await?;
    submit_prepared_agent_request_committed(graphql, &prepared).await
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedAgentRequest {
    pub(crate) create: AgentRequestCreate,
}

pub(crate) async fn prepare_agent_request(
    graphql: &str,
    agent_did: &str,
    content: &str,
    session_id: Option<&str>,
    behavior_id: Option<&str>,
    request_id: Option<String>,
    options: RequestSubmitOptions,
) -> Result<PreparedAgentRequest> {
    let (request_content, request_input) =
        content_and_input_with_prompt_selected_skill_ids(options.input, content);
    let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let behavior_id = resolve_request_behavior_id(graphql, agent_did, behavior_id).await?;
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
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent_did);
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
                gents::RequestIdentity {
                    request_id: request_id.clone(),
                    agent_did: agent_did.to_string(),
                    requester_did: None,
                    behavior_id: behavior_id.clone(),
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

/// Atomically establish a session goal and publish its first runnable request.
/// Exact retries use the same immutable submission key and converge to the
/// already-committed pair; conflicting retries fail without mutating either
/// document.
pub(crate) async fn create_goal_backed_agent_request(
    graphql: &str,
    agent_did: &str,
    content: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<SubmittedRequest> {
    use sha2::{Digest, Sha256};

    let objective = require_non_empty("goal-objective", objective)?.trim();
    anyhow::ensure!(
        token_budget.is_none_or(|budget| budget > 0),
        "goal-token-budget must be positive"
    );
    let digest = Sha256::digest(
        format!("{agent_did}\0{session_id}\0{objective}\0{token_budget:?}\0{content}").as_bytes(),
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
        gents::goal::deterministic_goal_creation_key(agent_did, session_id)
    );
    let prepared = prepare_agent_request(
        graphql,
        agent_did,
        content,
        Some(session_id),
        behavior_id,
        Some(request_id),
        RequestSubmitOptions {
            retry_key: Some(retry_key.clone()),
            ..Default::default()
        },
    )
    .await?;

    let access = gents::ConfigAccess::Graphql(graphql.to_string());
    gents::goal::submit_goal_backed_request(
        &access,
        agent_did,
        session_id,
        objective,
        token_budget,
        &prepared.create,
    )
    .await?;
    committed_submitted_request(graphql, &prepared.create).await
}

/// Embedded adapters keep their prompt identity and cancellation path while
/// delegating the goal/claim/request transaction to the runtime owner.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_goal_backed_agent_request_local(
    node: &defra_node::EmbeddedNode,
    actor: identity::Did,
    graphql: &str,
    agent_did: &str,
    objective: &str,
    token_budget: Option<i64>,
    session_id: &str,
    behavior_id: &str,
    request_id: String,
    mut options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    options.retry_key = Some(format!("goal-request:{request_id}"));
    let prepared = prepare_agent_request(
        graphql,
        agent_did,
        objective,
        Some(session_id),
        Some(behavior_id),
        Some(request_id),
        options,
    )
    .await?;
    gents::goal::submit_goal_backed_request_local(
        node,
        actor,
        agent_did,
        session_id,
        objective,
        token_budget,
        &prepared.create,
    )
    .await?;
    committed_submitted_request(graphql, &prepared.create).await
}

pub(crate) async fn submit_prepared_agent_request_committed(
    graphql: &str,
    prepared: &PreparedAgentRequest,
) -> Result<SubmittedRequest> {
    let access = gents::ConfigAccess::Graphql(graphql.to_string());
    let mutation = prepared
        .create
        .graphql_mutation()
        .map_err(anyhow::Error::msg)?;
    access
        .write_with_receipt("cli.request.submit", &mutation, || async {
            Ok(matching_prepared_receipt(graphql, &prepared.create)
                .await?
                .is_some())
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
    submitted_from_receipt(row)
}

async fn committed_submitted_request(
    graphql: &str,
    create: &AgentRequestCreate,
) -> Result<SubmittedRequest> {
    let row = read_submitted_receipt(graphql, create)
        .await?
        .context("committed signed request receipt is missing")?;
    submitted_from_receipt(row)
}

fn submitted_from_receipt(row: AgentRequestRow) -> Result<SubmittedRequest> {
    Ok(SubmittedRequest {
        request_doc_id: row
            .doc_id
            .filter(|id| !id.is_empty())
            .context("receipt has no physical identity")?,
        request_id: row.request_id,
        session_id: row.session_id.context("receipt has no session")?,
        agent_did: row.agent_did.context("receipt has no principal")?,
        requester_did: row.requester_did,
        behavior_id: row.behavior_id,
        input: row.input,
        created_at: row.created_at,
    })
}

pub(crate) async fn matching_prepared_receipt(
    graphql: &str,
    create: &AgentRequestCreate,
) -> Result<Option<AgentRequestRow>> {
    let Some(row) = read_submitted_receipt(graphql, create).await? else {
        return Ok(None);
    };
    let signature = bs58::encode(&create.admission.signature).into_string();
    anyhow::ensure!(
        row.admission_signer_did.as_deref() == Some(create.admission.signer_did.as_str())
            && row.admission_signature.as_deref() == Some(signature.as_str()),
        "submission receipt differs from prepared signed request"
    );
    Ok(Some(row))
}

async fn read_submitted_receipt(
    graphql: &str,
    create: &AgentRequestCreate,
) -> Result<Option<AgentRequestRow>> {
    let scope = gents::session::session_scope_filter(
        &create.agent_did,
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
    .context("reading immutable signed submission receipt")?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("request receipt query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous scoped submission receipt");
    let Some(value) = rows.first() else {
        return Ok(None);
    };
    let row: AgentRequestRow = serde_json::from_value(value.clone())?;
    anyhow::ensure!(
        row.request_id == create.request_id
            && row.agent_did.as_deref() == Some(create.agent_did.as_str())
            && row.session_id.as_deref() == Some(create.session_id.as_str())
            && row.requester_did.as_deref()
                == (!create.requester_did.is_empty()).then_some(create.requester_did.as_str()),
        "submission receipt crossed exact request scope"
    );
    gents::verify_request_receipt_signature(&row)?;
    Ok(Some(row))
}

async fn resolve_request_behavior_id(
    graphql: &str,
    agent_did: &str,
    requested: Option<&str>,
) -> Result<String> {
    let escaped_agent_did = gents::graphql::escape_graphql_string(agent_did);
    let response = post_graphql(
        graphql,
        &format!(
            r#"{{
                AgentPrincipal(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    limit: 2
                ) {{ agent_did default_behavior_id enabled }}
                AgentBehavior(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}
                ) {{ behavior_id agent_did enabled }}
            }}"#,
        ),
    )
    .await
    .context("loading authoritative request behavior")?;
    let principals = response
        .pointer("/data/AgentPrincipal")
        .and_then(Value::as_array)
        .context("AgentPrincipal query returned no row array")?;
    anyhow::ensure!(
        principals.len() == 1,
        "request target principal must resolve to exactly one row"
    );
    let principal = &principals[0];
    anyhow::ensure!(
        principal.get("enabled").and_then(Value::as_bool) == Some(true),
        "request target principal is disabled"
    );
    let behavior_id = match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(behavior_id) => behavior_id.to_string(),
        None => principal
            .get("default_behavior_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .context("request target principal has no canonical default behavior")?,
    };
    let behaviors = response
        .pointer("/data/AgentBehavior")
        .and_then(Value::as_array)
        .context("AgentBehavior query returned no row array")?;
    let matching = behaviors
        .iter()
        .filter(|row| row.get("behavior_id").and_then(Value::as_str) == Some(&behavior_id))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matching.len() == 1,
        "request behavior must resolve to exactly one row owned by the target principal"
    );
    anyhow::ensure!(
        matching[0].get("enabled").and_then(Value::as_bool) == Some(true),
        "request behavior is disabled"
    );
    Ok(behavior_id)
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
        // Retained partials are historical diagnostics, never current answer
        // progress. Loading/absence similarly carry no new bytes.
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

pub(crate) async fn wait_for_terminal_response(
    graphql: &str,
    request_id: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<RequestOutputEnvelope> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut last_progress_marker: Option<WaitProgressMarker> = None;

    let mut pinned: Option<AgentRequestRow> = None;
    loop {
        let request_row = {
            let query = request_terminal_query(
                request_id,
                pinned.as_ref().and_then(|row| row.doc_id.as_deref()),
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
            if let Some(original) = pinned.as_ref() {
                anyhow::ensure!(
                    request.agent_did == original.agent_did
                        && request.requester_did == original.requester_did
                        && request.session_id == original.session_id
                        && request.request_id == original.request_id,
                    "terminal request scope changed"
                );
            } else {
                pinned = Some(request.clone());
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
            Some(gents::session::CanonicalRequestOutput::Retracted) => {
                anyhow::bail!("canonical output for request {request_id} is retracted")
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
                    return request_output_envelope(request, output)
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
) -> Result<String> {
    let positional = positional.map(str::trim).filter(|value| !value.is_empty());
    let flag = flag.map(str::trim).filter(|value| !value.is_empty());
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => anyhow::bail!(
            "conflicting {noun} ids provided: positional={positional} and {flag_name}={flag}"
        ),
        (Some(request_id), _) | (_, Some(request_id)) => Ok(request_id.to_string()),
        (None, None) => anyhow::bail!("missing {noun} id"),
    }
}

pub(crate) fn resolve_request_id(positional: Option<&str>, flag: Option<&str>) -> Result<String> {
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

#[cfg(test)]
mod dual_id_tests {
    use super::*;

    #[test]
    fn resolve_dual_id_accepts_positional_only() {
        assert_eq!(
            resolve_dual_id("task", "--task-id", Some("task-1"), None).unwrap(),
            "task-1"
        );
    }

    #[test]
    fn resolve_dual_id_accepts_flag_only() {
        assert_eq!(
            resolve_dual_id("task", "--task-id", None, Some("task-1")).unwrap(),
            "task-1"
        );
    }

    #[test]
    fn resolve_dual_id_accepts_equal_positional_and_flag() {
        assert_eq!(
            resolve_dual_id("task", "--task-id", Some("task-1"), Some("task-1")).unwrap(),
            "task-1"
        );
    }

    #[test]
    fn resolve_dual_id_rejects_conflict() {
        let err = resolve_dual_id("task", "--task-id", Some("task-1"), Some("task-2"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("conflicting task ids provided"));
        assert!(err.contains("positional=task-1"));
        assert!(err.contains("--task-id=task-2"));
    }

    #[test]
    fn resolve_dual_id_rejects_missing_id() {
        let err = resolve_dual_id("task", "--task-id", None, None)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "missing task id");
    }
}

pub(crate) fn write_json_output_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    let contents =
        serde_json::to_vec_pretty(value).context("encoding JSON output for output file")?;
    fs::write(path, contents)
        .with_context(|| format!("writing JSON output file {}", path.display()))?;
    Ok(())
}

tokio::task_local! {
    /// Set while a command runs for an embedding caller (the desktop): its
    /// report is kept here instead of printed.
    static CAPTURED_REPORT: std::cell::RefCell<Option<Value>>;
}

pub(crate) fn print_json(value: &Value) -> Result<()> {
    if CAPTURED_REPORT
        .try_with(|report| *report.borrow_mut() = Some(value.clone()))
        .is_ok()
    {
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(value)?);
    io::stdout().flush()?;
    Ok(())
}

/// Runs `command` and returns the report it would have printed; the last
/// one when it prints several.
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

pub(crate) fn parse_duration_suffix(raw: &str) -> Result<Duration> {
    let s = raw.trim();
    if s.is_empty() {
        anyhow::bail!("duration must not be empty");
    }
    let split = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let (num_part, suffix) = s.split_at(split);
    let n: u64 = num_part
        .parse()
        .with_context(|| format!("invalid duration number in {raw}"))?;
    let secs = match suffix {
        "" | "s" => n,
        "m" => n.checked_mul(60).context("duration overflow")?,
        "h" => n.checked_mul(3600).context("duration overflow")?,
        "d" => n.checked_mul(86400).context("duration overflow")?,
        other => anyhow::bail!("unknown duration suffix {other:?} (use s, m, h, d)"),
    };
    Ok(Duration::from_secs(secs))
}

pub(crate) fn parse_valid_until_flag(raw: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    match raw.map(str::trim) {
        None => Ok(Some(Utc::now() + chrono::Duration::minutes(5))),
        Some("") | Some("none") | Some("0") => Ok(None),
        Some(value) => {
            let dur = parse_duration_suffix(value)?;
            let secs = i64::try_from(dur.as_secs()).context("duration too large")?;
            Ok(Some(Utc::now() + chrono::Duration::seconds(secs)))
        }
    }
}

pub(crate) async fn fetch_request_view(graphql: &str, request_id: &str) -> Result<AgentRequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 2
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
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
mod tests {
    use super::{
        content_and_input_with_prompt_selected_skill_ids, submit_prepared_agent_request_committed,
        PreparedAgentRequest, RequestSubmitOptions,
    };
    use axum::{
        body::{Body, Bytes},
        extract::State,
        response::{IntoResponse, Response},
        routing::post,
        Json, Router,
    };
    use futures_util::stream;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn canonical_live_content_projects_an_open_segment_before_closure() -> anyhow::Result<()>
    {
        use gents_protocol::output::{
            OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
        };
        use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

        let segment = OutputSegment {
            agent_did: "did:test:owner".into(),
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
            ordinal: Some(0),
            runs: vec![
                SegmentRun {
                    stream: 0,
                    bytes: 5,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::Text,
                    }),
                },
                SegmentRun {
                    stream: 1,
                    bytes: 4,
                    declaration: Some(StreamDeclaration {
                        block_index: 1,
                        part_index: 0,
                        payload: StreamPayload::Reasoning,
                    }),
                },
            ],
            payload: "helloidea".into(),
            close: None,
            created_at: "2026-09-01T00:00:00Z".into(),
        };
        let mut row = serde_json::to_value(segment)?;
        row["_docID"] = json!("segment-0");
        let owner = json!({
            "_docID": "physical", "request_id": "request",
            "agent_did": "did:test:owner", "requester_did": null,
            "session_id": "session", "lifecycle_state": "processing",
            "execution_generation": "generation",
            "execution_lease_secs": 30,
            "execution_lease_expires_at": "2026-09-01T00:00:30Z"
        });
        let app = Router::new().route(
            "/graphql",
            post(move || {
                let row = row.clone();
                let owner = owner.clone();
                async move {
                    Json(json!({"data": {"AgentRequest": [owner], "AgentOutputSegment": [row], "AgentMessage": []}}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/graphql", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let request = json!({
            "_docID": "physical",
            "request_id": "request",
            "agent_did": "did:test:owner",
            "requester_did": null,
            "session_id": "session",
            "lifecycle_state": "processing",
            "execution_generation": "generation",
            "execution_lease_secs": 30,
            "execution_lease_expires_at": "2026-09-01T00:00:30Z"
        });
        let request: gents_protocol::row::AgentRequestRow = serde_json::from_value(request)?;
        let result = super::observe_canonical_request_output(&endpoint, &request).await;
        server.abort();
        let gents::session::CanonicalRequestOutput::Live(presentation) = result? else {
            anyhow::bail!("expected canonical live output")
        };
        assert_eq!(presentation.body_markdown, "hello");
        assert_eq!(presentation.reasoning_markdown.as_deref(), Some("idea"));
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
                // reqwest retains this decode/transport error in the anyhow
                // cause chain, which recovery must classify before querying
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
            return Json(json!({"errors": [{"message": "database is locked"}]})).into_response();
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
        use gents::AgentIdentity;
        let dir = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
        let did = identity.did().to_string();
        let create = gents::build_signed_request(
            gents::RequestSpec::new(
                gents::RequestIdentity {
                    request_id: "stable-request-id".into(),
                    agent_did: did.clone(),
                    requester_did: None,
                    behavior_id: "behavior".into(),
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
            "_docID":"physical-receipt", "request_id":create.request_id, "agent_did":create.agent_did,
            "requester_did":create.requester_did, "behavior_id":create.behavior_id, "session_id":create.session_id,
            "content":create.content, "input":create.input, "execution_origin":create.execution_origin,
            "created_at":create.created_at, "retry_parent_request":create.retry_parent_request,
            "retry_root_request":create.retry_root_request, "retry_count":create.retry_count,
            "max_retries":create.max_retries, "subagent_depth":create.subagent_depth,
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
        let submitted =
            submit_prepared_agent_request_committed(&format!("http://{address}/"), &prepared)
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
        let submitted =
            submit_prepared_agent_request_committed(&format!("http://{address}/"), &prepared)
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
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
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

    #[test]
    fn terminal_query_uses_exact_physical_request_and_canonical_selection() {
        let query = super::request_terminal_query("label", Some("physical"));
        assert!(query.contains("AgentRequest("));
        assert!(query.contains("_docID:{_eq:\"physical\"}"));
        assert!(query.contains("terminal_output"));
        assert!(query.contains("execution_lease_expires_at"));
    }

    #[tokio::test]
    async fn prepared_request_signs_complete_client_semantics_once() -> anyhow::Result<()> {
        use gents::AgentIdentity;
        let dir = tempfile::tempdir()?;
        let identity = gents::KeyIdentity::load_or_create(dir.path().join("agent.key"), None)?;
        let did = identity.did().to_string();
        let data = serde_json::json!({"data": {
            "AgentPrincipal": [{"agent_did": did, "default_behavior_id": "default", "enabled": true}],
            "AgentBehavior": [{"agent_did": did, "behavior_id": "default", "enabled": true}]
        }});
        let app = axum::Router::new().route(
            "/graphql",
            axum::routing::post(move || async move { axum::Json(data) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/graphql", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let result = super::prepare_agent_request(
            &endpoint,
            &did,
            "/vuln-scan review this",
            Some("session"),
            None,
            Some("stable-request".into()),
            RequestSubmitOptions {
                caused_by_source_doc_id: Some("mailbox-doc".into()),
                input: Some(gents_protocol::request_input::RequestInput {
                    cwd: Some("/work".into()),
                    ..Default::default()
                }),
                valid_until: Some("2030-01-01T00:00:00Z".parse()?),
                retry_parent_request: Some("parent".into()),
                retry_parent_request_doc_id: Some("parent-doc".into()),
                retry_root_request: Some("root".into()),
                retry_key: Some("goal-submit:key".into()),
            },
        )
        .await;
        server.abort();
        let create = result?.create;
        assert_eq!(
            create.caused_by_source_doc_id.as_deref(),
            Some("mailbox-doc")
        );
        assert_eq!(create.request_id, "stable-request");
        assert_eq!(create.requester_did, did);
        assert_eq!(create.behavior_id, "default");
        assert_eq!(create.valid_until.as_deref(), Some("2030-01-01T00:00:00Z"));
        assert_eq!(create.retry_parent_request.as_deref(), Some("parent"));
        assert_eq!(
            create.retry_parent_request_doc_id.as_deref(),
            Some("parent-doc")
        );
        assert_eq!(create.retry_root_request.as_deref(), Some("root"));
        assert_eq!(create.retry_key.as_deref(), Some("goal-submit:key"));
        assert_eq!((create.retry_count, create.max_retries), (0, 3));
        assert_eq!(create.input.cwd.as_deref(), Some("/work"));
        assert_eq!(create.input.selected_skill_ids, ["vuln-scan"]);
        assert!(
            identity
                .verify(&did, &create.signing_payload(), &create.admission.signature)
                .await?
        );
        let mut rerouted = create.clone();
        rerouted.caused_by_source_doc_id = Some("another-mailbox-item".into());
        assert!(!matches!(
            identity
                .verify(
                    &did,
                    &rerouted.signing_payload(),
                    &rerouted.admission.signature
                )
                .await,
            Ok(true)
        ));
        Ok(())
    }

    #[test]
    fn retired_request_sampling_and_metadata_are_rejected() {
        for field in ["seed", "temperature", "metadata", "max_total_tokens"] {
            let input = json!({field: 1});
            assert!(
                serde_json::from_value::<gents_protocol::request_input::RequestInput>(input)
                    .is_err()
            );
        }
    }

    #[test]
    fn slash_prompt_adds_selected_skill_ids() {
        let (_, input) = content_and_input_with_prompt_selected_skill_ids(None, "/vuln-scan /work");
        assert_eq!(input.selected_skill_ids, ["vuln-scan"]);
    }

    #[test]
    fn slash_prompt_merges_skills_and_preserves_other_typed_input() {
        let (_, input) = content_and_input_with_prompt_selected_skill_ids(
            Some(gents_protocol::request_input::RequestInput {
                cwd: Some("/work".into()),
                selected_skill_ids: vec!["triage".into(), "vuln-scan".into()],
                ..Default::default()
            }),
            "/vuln-scan /work",
        );
        assert_eq!(input.cwd.as_deref(), Some("/work"));
        assert_eq!(input.selected_skill_ids, ["triage", "vuln-scan"]);
    }

    #[test]
    fn slash_prompt_strips_control_syntax_from_request_content() {
        let (content, input) =
            content_and_input_with_prompt_selected_skill_ids(None, "/vuln-scan\nReview /work");
        assert_eq!(content, "Review /work");
        assert_eq!(input.selected_skill_ids, ["vuln-scan"]);
    }
}
