use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::operations::request::{RequestOperationError, RequestRef, RequestWaitInput};
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::RegisteredIdentity;
use serde::Serialize;
use serde_json::Value;

use crate::operation_failure::OperationFailure;
use crate::require_non_empty;

/// The CLI's client identity, which signs the requests it sends.
pub(crate) fn client_signer(access: &ConfigAccess) -> Result<RegisteredIdentity> {
    let did = access
        .actor_did()
        .ok_or_else(|| OperationFailure::from(RequestOperationError::IdentityRequired))?;
    Ok(RegisteredIdentity::from_registered_did(did, None)?)
}

pub(crate) fn request_ref(node_did: &str, request_id: &str) -> RequestRef {
    RequestRef {
        node_did: node_did.to_owned(),
        requester_did: None,
        request_id: request_id.to_owned(),
    }
}

pub(crate) async fn wait_for_request<F, Fut>(
    access: &ConfigAccess,
    request: RequestRef,
    timeout_secs: u64,
    poll_secs: u64,
    progress: F,
) -> Result<RequestOutputEnvelope>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    let input = RequestWaitInput {
        request,
        timeout_secs,
        poll_secs,
    };
    let (row, output) = gents_session::operations::request::wait(access, input, progress)
        .await
        .map_err(OperationFailure::from)?;
    request_output_envelope(&row, output)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CliRequestMetadata {
    pub(crate) request_doc_id: String,
    pub(crate) request_id: String,
    pub(crate) node_did: String,
    pub(crate) requester_did: Option<String>,
    pub(crate) agent_did: Option<String>,
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

fn cli_presentation(value: gents_session::session::CanonicalPresentation) -> CliOutputPresentation {
    CliOutputPresentation {
        body_markdown: value.body_markdown,
        reasoning_markdown: value.reasoning_markdown,
    }
}

pub(crate) fn request_output_envelope(
    request: &AgentRequestRow,
    output: gents_session::session::CanonicalRequestOutput,
) -> Result<RequestOutputEnvelope> {
    let request = CliRequestMetadata {
        request_doc_id: request
            .doc_id
            .clone()
            .context("request output omitted physical request identity")?,
        request_id: request.request_id.clone(),
        node_did: request
            .node_did
            .clone()
            .context("request output omitted node_did")?,
        requester_did: request.requester_did.clone(),
        agent_did: request.agent_did.clone(),
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
        gents_session::session::CanonicalRequestOutput::Absent => CliOutputObservation::Absent,
        gents_session::session::CanonicalRequestOutput::Live(value) => CliOutputObservation::Live {
            presentation: cli_presentation(value),
        },
        gents_session::session::CanonicalRequestOutput::Settling(value) => {
            CliOutputObservation::Settling {
                presentation: cli_presentation(value),
            }
        }
        gents_session::session::CanonicalRequestOutput::Loading => CliOutputObservation::Loading,
        gents_session::session::CanonicalRequestOutput::Denied => CliOutputObservation::Denied,
        gents_session::session::CanonicalRequestOutput::Conflicted => {
            CliOutputObservation::Conflicted
        }
        gents_session::session::CanonicalRequestOutput::Invalid => CliOutputObservation::Invalid,
        gents_session::session::CanonicalRequestOutput::Retracted => {
            CliOutputObservation::Retracted
        }
        gents_session::session::CanonicalRequestOutput::RetainedPartial(_) => {
            CliOutputObservation::RetainedPartial
        }
        gents_session::session::CanonicalRequestOutput::Published {
            header,
            message,
            presentation,
        } => CliOutputObservation::Published {
            header,
            message,
            presentation: cli_presentation(presentation),
        },
        gents_session::session::CanonicalRequestOutput::TerminalNoMessage => {
            CliOutputObservation::TerminalNoMessage
        }
        gents_session::session::CanonicalRequestOutput::TerminalMessage {
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
    /// report is kept here instead of printed. Not request context: it is
    /// where the command's standard output goes, and it names no actor,
    /// request, session or transaction.
    static CAPTURED_REPORT: std::cell::RefCell<Option<Value>>;
}

pub(crate) fn print_json<T>(value: &T) -> Result<()>
where
    T: serde::Serialize + ?Sized,
{
    if CAPTURED_REPORT.try_with(|_| ()).is_ok() {
        let value = serde_json::to_value(value).context("encoding JSON report")?;
        CAPTURED_REPORT.with(|report| *report.borrow_mut() = Some(value));
        return Ok(());
    }
    // `println!` panics on a closed stdout; a report that cannot be written is
    // an ordinary error so long-running callers can still shut down in order.
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string_pretty(value)?)
        .and_then(|()| stdout.flush())
        .context("writing JSON report to stdout")
}

pub(crate) fn print_ndjson(value: &Value) -> Result<()> {
    if CAPTURED_REPORT
        .try_with(|report| *report.borrow_mut() = Some(value.clone()))
        .is_ok()
    {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string(value)?)
        .and_then(|()| stdout.flush())
        .context("writing NDJSON record to stdout")
}

pub(crate) async fn capture_report<F, E>(command: F) -> Result<Value, E>
where
    F: std::future::Future<Output = Result<(), E>>,
    E: From<anyhow::Error>,
{
    CAPTURED_REPORT
        .scope(std::cell::RefCell::new(None), async move {
            command.await?;
            CAPTURED_REPORT
                .with(|report| report.borrow_mut().take())
                .ok_or_else(|| anyhow::anyhow!("the command produced no report").into())
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

#[cfg(test)]
pub(crate) mod goal_input_tests;

/// The signed first request of a Goal a person starts; its content is the
/// objective.
pub(crate) async fn goal_request(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    agent: &str,
    objective: &str,
) -> anyhow::Result<gents_protocol::request_admission::AgentRequestCreate> {
    use gents_session::request::lifecycle::{
        build_signed_client_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
        RetryLink, DEFAULT_REQUEST_MAX_RETRIES,
    };
    let signer = client_signer(access)?;
    let actor = gents_store::identity::NodeIdentity::did(&signer).to_owned();
    let requester =
        gents_session::session::client_requester_scope(access, node, session, &actor).await?;
    let input = gents_session::request::lifecycle::prepare_user_message_input(
        access,
        node,
        session,
        gents_protocol::request_input::RequestInput {
            goal_assignment: Some(gents_protocol::request_input::GoalAssignmentInput {
                goal_id: gents_protocol::goal::deterministic_goal_id(node, session),
                root_request_doc_id: None,
            }),
            ..Default::default()
        },
        gents_protocol::request_input::QueueDelivery::Queue,
    )
    .await?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let spec = RequestSpec {
        input,
        retry_key: Some(format!("goal-request:{request_id}")),
        retry: Some(RetryLink {
            parent_request_id: None,
            parent_request_doc_id: None,
            root_request_id: request_id.clone(),
            retry_count: 0,
            max_retries: i64::from(DEFAULT_REQUEST_MAX_RETRIES),
        }),
        ..RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            RequestIdentity {
                request_id,
                node_did: node.to_owned(),
                requester_did: Some(requester),
                agent_did: agent.to_owned(),
                session_id: session.to_owned(),
                content: objective.to_owned(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at,
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(&actor),
        )
    };
    build_signed_client_request(access, spec, RequestSigner::Identity(&signer)).await
}
