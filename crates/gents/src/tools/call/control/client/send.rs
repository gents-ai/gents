use std::future::Future;
use std::time::Duration;

use gents_protocol::job::{JobControlKind, JobControlOutcome, JOB_CONTROL_PATH};
use gents_protocol::process_control::{
    ProcessControlOutcome, ToolControlIntent, ToolControlReceiptOutcome, PROCESS_CONTROL_PATH,
};
use serde::Deserialize;

use super::{ClientControlError, ClientControlObservationError};
use crate::store::config_client::{
    ConfigAccess, ConfigClientError, GraphqlEndpoint, TransactionFailure,
};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

const SUBMISSION_TIMEOUT: Duration = Duration::from_secs(15);
const OBSERVATION_DEADLINE: Duration = Duration::from_secs(30);

/// One signed control action and the runtime route that accepts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    Job(JobControlKind),
    ProcessStop,
}

impl ControlAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Job(JobControlKind::Stop) => "job_stop",
            Self::Job(JobControlKind::Pause) => "job_pause",
            Self::ProcessStop => "process_stop",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Job(_) => JOB_CONTROL_PATH,
            Self::ProcessStop => PROCESS_CONTROL_PATH,
        }
    }
}

/// Why a signed control did not return a verified agent receipt. Variants
/// before submission mean nothing was sent; `Refused` means the runtime
/// answered without saving it; `Unconfirmed` means it may still take effect.
#[derive(Debug, thiserror::Error)]
pub enum ControlSendError {
    #[error("connect to the owning runtime to control this invocation")]
    RuntimeRequired,
    #[error("the signed control intent is not authorized")]
    NotAuthorized,
    #[error("access was denied; check this client's permissions with the runtime owner")]
    PermissionDenied,
    #[error("the authenticated client cannot see this invocation")]
    NotFound,
    #[error("the control intent could not be prepared")]
    Unavailable(#[source] anyhow::Error),
    #[error("the runtime refused control intent {intent_id} (HTTP {status}): {detail}")]
    Refused {
        intent_id: String,
        status: u16,
        detail: String,
    },
    #[error("the selected agent rejected control {intent_doc_id}; check this client's permissions and the agent's state")]
    Rejected { intent_doc_id: String },
    #[error("control intent {intent_id} was not confirmed; the action may still take effect")]
    Unconfirmed {
        intent_id: String,
        intent_doc_id: Option<String>,
        #[source]
        source: anyhow::Error,
    },
}

impl ControlSendError {
    fn prepared(error: ClientControlError) -> Self {
        match error {
            ClientControlError::NotFound => Self::NotFound,
            ClientControlError::NotAuthorized | ClientControlError::WrongNode => {
                Self::NotAuthorized
            }
            ClientControlError::Unavailable(error)
                if TransactionFailure::config_client_error(&error)
                    .is_some_and(ConfigClientError::is_permission_denied) =>
            {
                Self::PermissionDenied
            }
            ClientControlError::Unavailable(error) => Self::Unavailable(error),
            error @ ClientControlError::Unconfirmed { .. } => Self::Unavailable(error.into()),
        }
    }
}

/// Stop or pause a plugin job and return its agent-signed outcome.
pub async fn control_job(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    node_did: &str,
    tool_call_doc_id: &str,
    kind: JobControlKind,
) -> Result<JobControlOutcome, ControlSendError> {
    match send_and_observe(
        access,
        signer,
        node_did,
        tool_call_doc_id,
        None,
        ControlAction::Job(kind),
    )
    .await?
    {
        (_, ToolControlReceiptOutcome::Job(outcome)) => Ok(outcome),
        (intent, _) => Err(unexpected_receipt(intent)),
    }
}

/// Stop a native background process and return its agent-signed outcome.
pub async fn control_process(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    node_did: &str,
    tool_call_doc_id: &str,
    expected: (&str, &str),
) -> Result<ProcessControlOutcome, ControlSendError> {
    match send_and_observe(
        access,
        signer,
        node_did,
        tool_call_doc_id,
        Some(expected),
        ControlAction::ProcessStop,
    )
    .await?
    {
        (_, ToolControlReceiptOutcome::Process(outcome)) => Ok(outcome),
        (intent, _) => Err(unexpected_receipt(intent)),
    }
}

fn unexpected_receipt(intent: ObservedIntent) -> ControlSendError {
    ControlSendError::Unconfirmed {
        intent_id: intent.intent_id,
        intent_doc_id: Some(intent.intent_doc_id),
        source: anyhow::anyhow!("the agent receipt names a different control kind"),
    }
}

struct ObservedIntent {
    intent_id: String,
    intent_doc_id: String,
}

async fn send_and_observe(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    node_did: &str,
    tool_call_doc_id: &str,
    expected: Option<(&str, &str)>,
    action: ControlAction,
) -> Result<(ObservedIntent, ToolControlReceiptOutcome), ControlSendError> {
    let ConfigAccess::Graphql(endpoint) = access else {
        return Err(ControlSendError::RuntimeRequired);
    };
    if endpoint.is_anonymous() {
        return Err(ControlSendError::NotAuthorized);
    }
    let url = control_url(endpoint, action)?;
    let intent = super::prepare(
        access,
        signer,
        node_did,
        tool_call_doc_id,
        expected,
        action.as_str(),
    )
    .await
    .map_err(ControlSendError::prepared)?;
    let request = endpoint
        .authorize(control_client().await?.post(url))
        .map_err(|error| ControlSendError::Unavailable(error.into()))?
        .timeout(SUBMISSION_TIMEOUT)
        .json(&intent);
    let intent_doc_id = submit(request, &intent).await?;
    let observed = ObservedIntent {
        intent_id: intent.intent_id,
        intent_doc_id,
    };
    let outcome = bounded(
        &observed,
        OBSERVATION_DEADLINE,
        observe(access, node_did, tool_call_doc_id, action, &observed),
    )
    .await?;
    match outcome {
        ToolControlReceiptOutcome::Rejected => Err(ControlSendError::Rejected {
            intent_doc_id: observed.intent_doc_id,
        }),
        ToolControlReceiptOutcome::Unconfirmed => Err(ControlSendError::Unconfirmed {
            intent_id: observed.intent_id,
            intent_doc_id: Some(observed.intent_doc_id),
            source: anyhow::anyhow!("the selected agent could not confirm the control outcome"),
        }),
        outcome => Ok((observed, outcome)),
    }
}

fn control_url(
    endpoint: &GraphqlEndpoint,
    action: ControlAction,
) -> Result<reqwest::Url, ControlSendError> {
    let mut url = reqwest::Url::parse(endpoint.url())
        .map_err(|error| ControlSendError::Unavailable(error.into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ControlSendError::Unavailable(anyhow::anyhow!(
            "runtime endpoint must be an HTTP URL without credentials"
        )));
    }
    let base = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .ok_or_else(|| {
            ControlSendError::Unavailable(anyhow::anyhow!(
                "runtime endpoint has no canonical GraphQL path"
            ))
        })?
        .to_owned();
    url.set_path(&format!("{base}{}", action.path()));
    Ok(url)
}

/// Redirects are refused because the signed bearer is scoped to this runtime.
async fn control_client() -> Result<&'static reqwest::Client, ControlSendError> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|error| ControlSendError::Unavailable(error.into()))
            })
            .await
            .map_err(|error| ControlSendError::Unavailable(error.into()))?
        })
        .await
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Accepted {
    Queued { intent_doc_id: String },
}

#[derive(Deserialize)]
struct Failure {
    error: String,
}

/// Every answer after the request leaves the client is either an explicit
/// refusal that saved nothing or an unconfirmed intent.
async fn submit(
    request: reqwest::RequestBuilder,
    intent: &ToolControlIntent,
) -> Result<String, ControlSendError> {
    let unconfirmed = |source: anyhow::Error| ControlSendError::Unconfirmed {
        intent_id: intent.intent_id.clone(),
        intent_doc_id: None,
        source,
    };
    let response = request
        .send()
        .await
        .map_err(|error| unconfirmed(error.into()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| unconfirmed(error.into()))?;
    if status.is_success() {
        return match serde_json::from_slice::<Accepted>(&body) {
            Ok(Accepted::Queued { intent_doc_id }) => Ok(intent_doc_id),
            Err(error) => Err(unconfirmed(
                anyhow::Error::new(error)
                    .context(format!("runtime returned HTTP {status} without a queued intent")),
            )),
        };
    }
    let detail = serde_json::from_slice::<Failure>(&body)
        .map(|failure| failure.error)
        .unwrap_or_else(|_| format!("runtime returned HTTP {status}"));
    if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT {
        return Err(unconfirmed(anyhow::anyhow!(
            "runtime returned HTTP {status}: {detail}"
        )));
    }
    Err(match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            ControlSendError::NotAuthorized
        }
        reqwest::StatusCode::NOT_FOUND => ControlSendError::NotFound,
        status => ControlSendError::Refused {
            intent_id: intent.intent_id.clone(),
            status: status.as_u16(),
            detail,
        },
    })
}

async fn bounded<F>(
    intent: &ObservedIntent,
    deadline: Duration,
    future: F,
) -> Result<ToolControlReceiptOutcome, ControlSendError>
where
    F: Future<Output = Result<ToolControlReceiptOutcome, ControlSendError>>,
{
    tokio::time::timeout(deadline, future)
        .await
        .unwrap_or_else(|_| {
            Err(ControlSendError::Unconfirmed {
                intent_id: intent.intent_id.clone(),
                intent_doc_id: Some(intent.intent_doc_id.clone()),
                source: anyhow::anyhow!("no agent receipt before the observation deadline"),
            })
        })
}

/// Arms both receipt sources before each snapshot, as the subscription owner
/// requires, and verifies every candidate through `read_receipt`.
async fn observe(
    access: &ConfigAccess,
    node_did: &str,
    tool_call_doc_id: &str,
    action: ControlAction,
    intent: &ObservedIntent,
) -> Result<ToolControlReceiptOutcome, ControlSendError> {
    let unconfirmed = |source: anyhow::Error| ControlSendError::Unconfirmed {
        intent_id: intent.intent_id.clone(),
        intent_doc_id: Some(intent.intent_doc_id.clone()),
        source,
    };
    let escaped = escape_graphql_string(&intent.intent_doc_id);
    let mut controls = access
        .subscribe(&format!(
            r#"subscription {{ AgentToolControl(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .map_err(|error| unconfirmed(error.into()))?;
    let mut claims = access
        .subscribe(&format!(
            r#"subscription {{ AgentToolControlClaim(filter: {{ intent_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .map_err(|error| unconfirmed(error.into()))?;
    loop {
        match super::read_receipt(
            access,
            node_did,
            &intent.intent_doc_id,
            tool_call_doc_id,
            action.as_str(),
        )
        .await
        {
            Ok(Some(receipt)) => return Ok(receipt.outcome),
            Ok(None) => {}
            Err(ClientControlObservationError::Unavailable(error)) => {
                return Err(unconfirmed(error))
            }
        }
        let event = tokio::select! {
            event = controls.recv() => event,
            event = claims.recv() => event,
        };
        match event {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(unconfirmed(anyhow::anyhow!(
                    "receipt observation closed before an agent receipt arrived"
                )))
            }
            Err(error) => return Err(unconfirmed(error.into())),
        }
    }
}

#[cfg(test)]
mod tests;
