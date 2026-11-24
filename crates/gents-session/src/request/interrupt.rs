use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use tokio::sync::watch;

use gents_store::graphql::escape_graphql_string;

mod scoped;

pub const INTERRUPT_DRAIN_REASON: &str =
    "automated wake-up drained because active request was interrupted";

pub use scoped::{observe_request_interrupted_at_with_access, ScopedInterruptObservation};

use crate::session::interrupt_marker::{self, InterruptMarker};

/// Why an interrupt was refused (Lean `Identity.SessionInterrupt`: who may
/// interrupt a session is who may update it).
#[derive(Debug, thiserror::Error)]
pub enum SessionInterruptRefusal {
    #[error("this caller may not interrupt session {session_id}: only the session's working Agent and the administrators of its Agent may interrupt it. To reach it, send it a message with agent_message without interrupt")]
    NotPermitted { session_id: String },
    #[error("session {session_id} has no interrupt marker, so it cannot be interrupted; check the session with agent_session_read")]
    NoMarker { session_id: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SessionInterruptError {
    #[error(transparent)]
    Refused(#[from] SessionInterruptRefusal),
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

/// A committed session interrupt: the marker's new generation and when, and
/// the request its unvalidated scan saw running, which only labels the
/// receipt (the generation, not the scan, decides what is interrupted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInterrupted {
    pub generation: i64,
    pub interrupted_at: String,
    pub active_request_id: Option<String>,
}

/// What a request interrupt found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestInterrupt {
    /// The session's marker passes the request's claim; `already` when an
    /// earlier interrupt had passed it.
    Interrupted { at: Option<String>, already: bool },
    /// The request was pending and is interrupted before its claim.
    InterruptedBeforeClaim,
    /// The request had already settled.
    Settled,
}

/// Interrupts a session (Lean `InterruptGeneration`): reads the session's
/// marker by id, writes one past its generation and stages the session-halt
/// hook. It writes no request and decides on no scan, so only another
/// interrupt of the session aborts it. The request active at its commit and
/// the automated wakes pending at its commit recorded the generation it
/// passed; pending user input runs as the next turn.
pub async fn interrupt_session_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<std::result::Result<SessionInterrupted, SessionInterruptRefusal>> {
    let refused = || SessionInterruptRefusal::NotPermitted {
        session_id: session_id.to_owned(),
    };
    let Some(session) = gents_store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        node_did,
        session_id,
        requester_did,
    )
    .await?
    else {
        return Ok(Err(refused()));
    };
    let Some(marker) = interrupt_marker::read_in_txn(txn, &session.doc_id).await? else {
        return Ok(Err(SessionInterruptRefusal::NoMarker {
            session_id: session_id.to_owned(),
        }));
    };
    let Some(bumped) = bump_in_txn(txn, &marker).await? else {
        return Ok(Err(refused()));
    };
    crate::request::hooks::points::session_halt(txn, node_did, session_id).await?;
    let active_request_id = active_session_request_in_txn(txn, session_id, node_did, requester_did)
        .await?;
    Ok(Ok(SessionInterrupted {
        generation: bumped.generation,
        interrupted_at: bumped.interrupted_at.unwrap_or_default(),
        active_request_id,
    }))
}

async fn bump_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    marker: &InterruptMarker,
) -> Result<Option<InterruptMarker>> {
    interrupt_marker::bump_in_txn(txn, marker, &Utc::now().to_rfc3339()).await
}

/// The session interrupt the stop and message owners run inside their own
/// transaction; a refusal is their error.
pub async fn interrupt_active_session_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    Ok(
        interrupt_session_in_txn(txn, session_id, node_did, requester_did)
            .await??
            .active_request_id,
    )
}

/// Interrupts a request as its working Agent, signed by that Agent's managed
/// key, selecting only a request naming that Agent.
pub async fn interrupt_request(
    node: &Arc<EmbeddedNode>,
    agent: &dyn gents_store::identity::NodeIdentity,
    request_id: &str,
) -> Result<()> {
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent,
    )?;
    let logical = escape_graphql_string(request_id);
    let owner = escape_graphql_string(agent.did());
    let filter = format!("request_id:{{_eq:\"{logical}\"}},agent_did:{{_eq:\"{owner}\"}}");
    access
        .transact_signed_conflict_rebuild("interrupt.request", |txn| {
            let filter = &filter;
            Box::pin(async move { interrupt_request_matching_in_txn(txn, filter).await })
        })
        .await?
        .map(|_| ())
        .map_err(Into::into)
}

/// Interrupts the exact scoped request as its working Agent. An Agent-owned
/// request accepts only that Agent's signed update, so the caller supplies the
/// Agent's managed signer; without one the interrupt is refused rather than
/// attempted as another actor.
pub async fn interrupt_request_by_doc_id(
    node: &Arc<EmbeddedNode>,
    signer: Option<&gents_store::identity::ManagedAgentSigner>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let signer =
        signer.ok_or(gents_store::config_client::ConfigClientError::ManagedAgentSignerRequired)?;
    let access = gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(signer.did())?,
        signer.clone(),
    )?;
    interrupt_request_by_doc_id_with_access(&access, request_doc_id, node_did, requester_did)
        .await
        .map(|_| ())
        .map_err(Into::into)
}

fn exact_request_filter(
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty() && !node_did.trim().is_empty(),
        "interrupt requires physical request and node identity"
    );
    let physical = escape_graphql_string(request_doc_id);
    let owner = escape_graphql_string(node_did);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "_docID:{{_eq:\"{physical}\"}},node_did:{{_eq:\"{owner}\"}},requester_did:{{_eq:{requester}}}"
    ))
}

fn scoped_request_filter(
    request_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !request_id.trim().is_empty() && !node_did.trim().is_empty(),
        "interrupt fetch requires logical request and node identity"
    );
    let logical = escape_graphql_string(request_id);
    let owner = escape_graphql_string(node_did);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "request_id:{{_eq:\"{logical}\"}},node_did:{{_eq:\"{owner}\"}},requester_did:{{_eq:{requester}}}"
    ))
}

/// The interrupt is transaction-only, so a confirmed conflict rebuilds it from
/// fresh rows (InterruptLatchContention.tla).
pub async fn interrupt_request_by_doc_id_with_access(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> std::result::Result<RequestInterrupt, SessionInterruptError> {
    let filter = exact_request_filter(request_doc_id, node_did, requester_did)?;
    Ok(access
        .transact_signed_conflict_rebuild("interrupt.request", |txn| {
            let filter = &filter;
            Box::pin(async move { interrupt_request_matching_in_txn(txn, filter).await })
        })
        .await??)
}

/// Interrupts the exact scoped request inside the caller's transaction, so a
/// job's terminal decision and its child interrupts share one commit.
pub async fn interrupt_request_by_doc_id_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let filter = exact_request_filter(request_doc_id, node_did, requester_did)?;
    interrupt_request_matching_in_txn(txn, &filter).await??;
    Ok(())
}

/// The request is read by id, so a claim or terminal committed meanwhile
/// aborts this transaction and it is rebuilt from the new row: a pending
/// request is interrupted before its claim by a write of its own row; a
/// running one by passing the generation its claim recorded.
async fn interrupt_request_matching_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    filter: &str,
) -> Result<std::result::Result<RequestInterrupt, SessionInterruptRefusal>> {
    let lookup = txn.execute(&format!(r#"{{AgentRequest(filter: {{{filter}}}, limit: 2) {{_docID request_id session_id node_did requester_did lifecycle_state interrupt_generation}}}}"#)).await?;
    let rows = lookup["data"]["AgentRequest"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("interrupt request query omitted rows"))?;
    anyhow::ensure!(
        rows.len() == 1,
        "interrupt request is missing or ambiguous within selected scope"
    );
    let row = &rows[0];
    let request_id = row["request_id"]
        .as_str()
        .context("interrupt request missing logical identity")?;
    let physical = row["_docID"]
        .as_str()
        .context("interrupt request missing physical identity")?;
    let session_id = row["session_id"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .context("interrupt request missing session identity")?;
    let node_did = row["node_did"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .context("interrupt request missing node identity")?;
    let state = row["lifecycle_state"]
        .as_str()
        .map(RequestLifecycleState::parse)
        .transpose()?;
    match state {
        Some(state) if state.is_terminal() => Ok(Ok(RequestInterrupt::Settled)),
        Some(RequestLifecycleState::Pending) => {
            interrupt_pending_in_txn(txn, physical, node_did, request_id, "interrupted before claim")
                .await?;
            crate::request::hooks::points::session_halt(txn, node_did, session_id).await?;
            Ok(Ok(RequestInterrupt::InterruptedBeforeClaim))
        }
        _ => {
            let Some(marker) =
                interrupt_marker::read_for_session_in_txn(
                    txn,
                    node_did,
                    session_id,
                    row["requester_did"].as_str(),
                )
                .await?
            else {
                return Ok(Err(SessionInterruptRefusal::NoMarker {
                    session_id: session_id.to_owned(),
                }));
            };
            let recorded = row["interrupt_generation"].as_i64();
            if interrupt_marker::running_interrupted(recorded, marker.generation) {
                return Ok(Ok(RequestInterrupt::Interrupted {
                    at: marker.interrupted_at,
                    already: true,
                }));
            }
            let Some(bumped) = bump_in_txn(txn, &marker).await? else {
                return Ok(Err(SessionInterruptRefusal::NotPermitted {
                    session_id: session_id.to_owned(),
                }));
            };
            crate::request::hooks::points::session_halt(txn, node_did, session_id).await?;
            Ok(Ok(RequestInterrupt::Interrupted {
                at: bumped.interrupted_at,
                already: false,
            }))
        }
    }
}

/// Moves a pending request to interrupted by a write of its own row, which a
/// concurrent claim also writes, and stages its terminal hook. Returns
/// whether the row was still pending.
pub(crate) async fn interrupt_pending_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    node_did: &str,
    request_id: &str,
    reason: &str,
) -> Result<bool> {
    let transition =
        RequestLifecycleState::Pending.transition_to(RequestLifecycleState::Interrupted)?;
    let terminalized_at = Utc::now().to_rfc3339();
    let response = txn
        .execute_client_signed_update(
            &format!(
                r#"mutation($input: AgentRequestMutationInputArg!) {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, node_did: {{ _eq: "{node}" }}, lifecycle_state: {{ _eq: "{pending}" }} }}, input: $input) {{ _docID workspace_id workspace_owner_node_did }} }}"#,
                doc = escape_graphql_string(doc_id),
                node = escape_graphql_string(node_did),
                pending = transition.from().as_str(),
            ),
            &serde_json::json!({"input": {
                "lifecycle_state": transition.to().as_str(),
                "failure_reason": reason,
                "terminalized_at": terminalized_at,
                "terminal_redrive_attempts": 0,
            }}),
            gents_protocol::schemas::AGENT_REQUEST,
        )
        .await?;
    let rows = match &response["data"]["update_AgentRequest"] {
        serde_json::Value::Array(rows) => rows.clone(),
        serde_json::Value::Null => Vec::new(),
        row @ serde_json::Value::Object(_) => vec![row.clone()],
        _ => anyhow::bail!("pending interrupt mutation returned unexpected rows"),
    };
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    anyhow::ensure!(
        rows.len() == 1 && row["_docID"].as_str() == Some(doc_id),
        "pending interrupt updated another physical request"
    );
    crate::request::workspace_binding::release_terminal_writer_binding(txn, row).await?;
    crate::request::hooks::points::request_terminal(
        txn,
        node_did,
        request_id,
        transition.to(),
        reason,
        &terminalized_at,
    )
    .await?;
    Ok(true)
}

/// Records on a wake this transaction published the generation of the
/// marker it read by id (`InterruptGeneration`): an interrupt committing
/// first aborts the publication, one committing later passes the record.
pub(crate) async fn record_wake_generation_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    wake_doc_id: &str,
    marker: Option<&InterruptMarker>,
) -> Result<()> {
    let Some(marker) = marker else {
        return Ok(());
    };
    let response = txn
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ interrupt_generation: {} }}) {{ _docID }} }}"#,
            escape_graphql_string(wake_doc_id),
            marker.generation
        ))
        .await?;
    anyhow::ensure!(
        response["data"]["update_AgentRequest"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "wake publication did not record its interrupt generation"
    );
    Ok(())
}

async fn active_session_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    let rendered = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, session_scope, Filter, Query, Selection,
        };
        Query::select(Selection::doc_id().field(fields::REQUEST_ID))
            .filter(session_scope(node_did, session_id, requester_did))
            .filter(Filter::eq(fields::PURPOSE, "normal"))
            .filter(Filter::any_of(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::unsettled_execution().map(RequestLifecycleState::as_str),
            ))
            .limit(2)
            .render()
    };
    let response = txn.run(&rendered).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("active session request query omitted rows")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active requests in exact session scope"
    );
    rows.first()
        .map(|row| {
            row["request_id"]
                .as_str()
                .map(str::to_owned)
                .context("active session request omitted request_id")
        })
        .transpose()
}

pub async fn active_session_request_with_access(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::row::AgentRequestRow>> {
    let query = active_session_request_query(session_id, node_did, requester_did);
    let response = access.execute(&query).await?;
    active_session_request_rows(&response["data"])
}

fn active_session_request_query(
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> String {
    let scope = gents_protocol::session::session_scope_filter(node_did, session_id, requester_did);
    format!(
        r#"{{AgentRequest(filter:{{{scope},purpose:{{_eq:"normal"}},lifecycle_state:{{_in:{claimed_or_processing}}}}},limit:2){{_docID request_id node_did requester_did session_id lifecycle_state}}}}"#,
        claimed_or_processing = RequestLifecycleState::unsettled_execution_graphql_list()
    )
}

fn active_session_request_rows(
    data: &serde_json::Value,
) -> Result<Option<gents_protocol::row::AgentRequestRow>> {
    let mut rows: Vec<gents_protocol::row::AgentRequestRow> = serde_json::from_value(
        data.get("AgentRequest")
            .context("active request query omitted rows")?
            .clone(),
    )?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active physical requests in exact session scope"
    );
    Ok(rows.pop())
}

/// When the session's interrupt passed the generation the request's claim
/// recorded, or `None` while it has not.
pub async fn request_interrupted_at_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    recorded: Option<i64>,
) -> Result<Option<String>> {
    if recorded.is_none() {
        return Ok(None);
    }
    Ok(
        interrupt_marker::read_for_session_in_txn(txn, node_did, session_id, requester_did)
            .await?
            .filter(|marker| interrupt_marker::running_interrupted(recorded, marker.generation))
            .map(|marker| marker.interrupted_at.unwrap_or_default()),
    )
}

/// [`request_interrupted_at_in_txn`] for a request in `state`: one that
/// settled other than interrupted was not active at an interrupt that passes
/// its record later (`InterruptGeneration.observedInterrupted`).
pub async fn observed_interrupted_at_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    state: Option<RequestLifecycleState>,
    recorded: Option<i64>,
) -> Result<Option<String>> {
    if interrupt_marker::settled_otherwise(state) {
        return Ok(None);
    }
    request_interrupted_at_in_txn(txn, node_did, session_id, requester_did, recorded).await
}

/// Whether the session's interrupt passed the generation `row` recorded,
/// read by id so an interrupt committing meanwhile aborts the caller.
pub async fn row_interrupted_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    row: &gents_protocol::row::AgentRequestRow,
) -> Result<bool> {
    let (Some(node_did), Some(session_id)) = (row.node_did.as_deref(), row.session_id.as_deref())
    else {
        return Ok(false);
    };
    Ok(observed_interrupted_at_in_txn(
        txn,
        node_did,
        session_id,
        row.requester_did.as_deref(),
        row.lifecycle_state,
        row.interrupt_generation,
    )
    .await?
    .is_some())
}

/// [`row_interrupted_in_txn`] in a read-only transaction of its own.
pub async fn row_interrupted_with_access(
    access: &gents_store::config_client::ConfigAccess,
    row: &gents_protocol::row::AgentRequestRow,
) -> Result<bool> {
    access
        .transact_readonly("interrupt.observe_row", |txn| {
            Box::pin(async move { row_interrupted_in_txn(txn, row).await })
        })
        .await
}

/// [`request_interrupted_at_in_txn`] for the request `request_doc_id`.
pub async fn request_interrupted_at_by_doc_id_with_access(
    access: &gents_store::config_client::ConfigAccess,
    request_doc_id: &str,
) -> Result<Option<String>> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "interrupt fetch requires a physical request document id"
    );
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ node_did session_id requester_did lifecycle_state interrupt_generation }} }}"#,
        escape_graphql_string(request_doc_id)
    );
    access
        .transact_readonly("interrupt.observe_request", |txn| {
            let query = &query;
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let rows = response["data"]["AgentRequest"]
                    .as_array()
                    .context("interrupt request fetch omitted rows")?;
                anyhow::ensure!(rows.len() <= 1, "interrupt request fetch is ambiguous");
                let Some(row) = rows.first() else {
                    return Ok(None);
                };
                observed_interrupted_at_in_txn(
                    txn,
                    row["node_did"].as_str().unwrap_or_default(),
                    row["session_id"].as_str().unwrap_or_default(),
                    row["requester_did"].as_str(),
                    RequestLifecycleState::parse_opt(row["lifecycle_state"].as_str()),
                    row["interrupt_generation"].as_i64(),
                )
                .await
            })
        })
        .await
}

#[derive(Debug, Clone)]
pub struct InterruptIntent {
    pub at: DateTime<Utc>,
}

const OBSERVER_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn an observer task that signals the channel once the session's
/// interrupt marker passes the generation `request`'s claim recorded. The
/// marker is re-read when it changes (local write or replicated merge) and on
/// a slower fallback tick in case the change subscription overflows or
/// closes. Exits when signaled once, the shutdown receiver changes, or the
/// handle is aborted.
pub fn spawn_request_interrupt_observer(
    node: Arc<EmbeddedNode>,
    access: gents_store::config_client::ConfigAccess,
    request: &gents_protocol::row::AgentRequest,
    interrupt_tx: watch::Sender<Option<InterruptIntent>>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let changes = gents_store::subscription_source::subscribe_collections(
        &node,
        [gents_protocol::schemas::AGENT_SESSION_INTERRUPT_NAME],
    )
    .inspect_err(|error| {
        tracing::warn!(doc_id = %request.doc_id, %error, "interrupt observer polling only");
    })
    .ok();
    let watched = WatchedRequest {
        doc_id: request.doc_id.clone(),
        node_did: request.node_did.clone(),
        session_id: request.session_id.clone(),
        requester_did: request.requester_did.clone(),
        recorded: request.interrupt_generation,
    };
    tokio::spawn(async move {
        let Some(marker) = watched.marker(&access).await else {
            let _ = shutdown.changed().await;
            return;
        };
        observe_request_interrupt(
            &marker,
            changes,
            || watched.read(&access),
            &interrupt_tx,
            &mut shutdown,
        )
        .await;
    })
}

struct WatchedRequest {
    doc_id: String,
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
    recorded: Option<i64>,
}

impl WatchedRequest {
    async fn marker(&self, access: &gents_store::config_client::ConfigAccess) -> Option<String> {
        let read = access
            .transact_readonly("interrupt.observer_marker", |txn| {
                Box::pin(async move {
                    interrupt_marker::read_for_session_in_txn(
                        txn,
                        &self.node_did,
                        &self.session_id,
                        self.requester_did.as_deref(),
                    )
                        .await
                })
            })
            .await;
        match read {
            Ok(marker) => marker.map(|marker| marker.doc_id),
            Err(error) => {
                tracing::warn!(doc_id = %self.doc_id, %error, "interrupt observer cannot find the session's interrupt marker");
                None
            }
        }
    }

    async fn read(&self, access: &gents_store::config_client::ConfigAccess) -> Option<String> {
        let read = access
            .transact_readonly("interrupt.observer_read", |txn| {
                Box::pin(async move {
                    request_interrupted_at_in_txn(
                        txn,
                        &self.node_did,
                        &self.session_id,
                        self.requester_did.as_deref(),
                        self.recorded,
                    )
                        .await
                })
            })
            .await;
        match read {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(doc_id = %self.doc_id, error = %error, "interrupt observer query failed; will retry");
                None
            }
        }
    }
}

trait InterruptChangeFeed: Send {
    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<events::DocumentChangeBatch>> + Send;
}

impl InterruptChangeFeed for gents_store::subscription_source::ChangeSubscription {
    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<events::DocumentChangeBatch>> + Send {
        gents_store::subscription_source::ChangeSubscription::recv(self)
    }
}

/// The fallback tick is polled before the change feed so a feed that is
/// always ready with unrelated documents cannot starve the initial read or
/// the periodic re-read.
async fn observe_request_interrupt<F, R, Fut>(
    watched_doc_id: &str,
    mut changes: Option<F>,
    mut read: R,
    interrupt_tx: &watch::Sender<Option<InterruptIntent>>,
    shutdown: &mut watch::Receiver<bool>,
) where
    F: InterruptChangeFeed,
    R: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let mut ticker = tokio::time::interval(OBSERVER_POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            _ = ticker.tick() => {}
            batch = async {
                match changes.as_mut() {
                    Some(changes) => changes.recv().await,
                    None => std::future::pending().await,
                }
            } => match batch {
                Some(batch)
                    if batch.resync_required
                        || batch
                            .changes
                            .iter()
                            .any(|change| change.doc_id == watched_doc_id) => {}
                Some(_) => continue,
                None => {
                    tracing::warn!(
                        doc_id = %watched_doc_id,
                        "interrupt observer change subscription closed; polling only"
                    );
                    changes = None;
                    continue;
                }
            },
        }
        if interrupt_tx.borrow().is_some() {
            return;
        }
        let Some(at_str) = read().await else {
            continue;
        };
        match chrono::DateTime::parse_from_rfc3339(&at_str) {
            Ok(dt) => {
                let intent = InterruptIntent {
                    at: dt.with_timezone(&Utc),
                };
                let _ = interrupt_tx.send(Some(intent));
                tracing::info!(
                    marker_doc_id = %watched_doc_id,
                    interrupt_at = %dt.to_rfc3339(),
                    "interrupt observer latched; signaled daemon"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    doc_id = %watched_doc_id,
                    bad_value = %at_str,
                    error = %e,
                    "invalid interrupted_at; observer continuing"
                );
            }
        }
    }
}

#[cfg(test)]
mod observer_scheduling_tests;

#[cfg(test)]
mod physical_scope_tests;

#[cfg(test)]
mod queue_tests;
