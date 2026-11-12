//! Owed lifecycle deliveries (Lean `Hooks.OwedDelivery`): the record a
//! must-commit point publishes for a plugin whose staging was dropped, the
//! redrive that runs that plugin's hook later, the halts re-derived from core
//! state, and the pruning of settled records.

use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::document_query::{
    fields::agent_request as request_fields, fields::agent_session as session_fields,
    fields::owed_delivery as fields, Delete, Direction, Filter, Input, Query, Selection, Update,
};
use gents_protocol::owed_delivery::{
    delivery_key, OwedDeliveryRow, OwedSettlement, OwedSubject, OWED_DELIVERY_FIELDS,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::session::SessionOwnerRow;
use gents_store::config_client::agent_session::load_agent_session_owner_in_txn;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_store::identity::NodeIdentity;

use super::native::TxnHost;
use super::registry::{OperationContext, Registry, RequestTerminal, SessionHalt};
use super::stage::{stage, HookError, Plugin, Staged};

/// Lean `Hooks.OwedDelivery.Key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub point: String,
    pub plugin: String,
    pub subject: String,
}

/// Lean `Hooks.OwedDelivery.Kind`: a must-commit staging runs as the Agent
/// owning its subject session; a delivery that asks core to act for someone
/// runs as its recorded actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Staging,
    OnBehalf,
}

impl Kind {
    /// Lean `Hooks.OwedDelivery.Kind.ofPoint`.
    pub fn of_point(point: &str) -> Self {
        match point {
            super::points::REQUEST_TERMINAL_STAGE | super::points::CONTROL_SESSION_HALT => {
                Self::Staging
            }
            _ => Self::OnBehalf,
        }
    }
}

/// Lean `Hooks.OwedDelivery.owe`: the identity a delivery would run as, or
/// `None` when it is not recorded.
pub fn owe_runner<F>(
    kind: Kind,
    actor: Option<&str>,
    owner: Option<&str>,
    signable: F,
) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    match kind {
        Kind::Staging => owner.map(str::to_owned),
        Kind::OnBehalf => actor.filter(|actor| signable(actor)).map(str::to_owned),
    }
}

/// Lean `Hooks.OwedDelivery.Settlement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Delivered,
    Dropped,
}

/// Lean `Hooks.OwedDelivery.Record`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub key: Key,
    pub kind: Kind,
    pub actor: Option<String>,
    pub runner: String,
    pub attempts: u64,
    pub last_error: Option<String>,
    pub settled: Option<Settlement>,
}

/// Lean `Hooks.OwedDelivery.Observation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub authorized: bool,
    pub current: bool,
}

/// Lean `Hooks.OwedDelivery.Write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub subject: String,
    pub doc: String,
}

/// Lean `Hooks.OwedDelivery.Attempt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    Applied(Vec<Write>),
    Failed(String),
}

const REFUSED_WRITE: &str = "a write was refused";

impl Record {
    /// Lean `Hooks.OwedDelivery.Record.fresh`.
    pub fn fresh(kind: Kind, actor: Option<&str>, runner: &str, key: Key) -> Self {
        Self {
            key,
            kind,
            actor: actor.map(str::to_owned),
            runner: runner.to_owned(),
            attempts: 0,
            last_error: None,
            settled: None,
        }
    }

    pub fn pending(&self) -> bool {
        self.settled.is_none()
    }

    /// Lean `Hooks.OwedDelivery.admissible`.
    pub fn admissible<F>(&self, may_write: F, writes: &[Write]) -> bool
    where
        F: Fn(&Write) -> bool,
    {
        writes
            .iter()
            .all(|write| write.subject == self.key.subject && may_write(write))
    }

    /// Lean `Hooks.OwedDelivery.redrive`.
    pub fn redrive<F>(&self, observed: Observation, attempt: &Attempt, may_write: F) -> Self
    where
        F: Fn(&Write) -> bool,
    {
        if self.settled.is_some() {
            return self.clone();
        }
        if !observed.authorized || !observed.current {
            return Self {
                settled: Some(Settlement::Dropped),
                ..self.clone()
            };
        }
        let failed = |error: &str| Self {
            attempts: self.attempts + 1,
            last_error: Some(error.to_owned()),
            ..self.clone()
        };
        match attempt {
            Attempt::Applied(writes) if self.admissible(may_write, writes) => Self {
                settled: Some(Settlement::Delivered),
                ..self.clone()
            },
            Attempt::Applied(_) => failed(REFUSED_WRITE),
            Attempt::Failed(error) => failed(error),
        }
    }

    /// Lean `Hooks.OwedDelivery.performs`.
    pub fn performs<F>(&self, observed: Observation, attempt: &Attempt, may_write: F) -> bool
    where
        F: Fn(&Write) -> bool,
    {
        self.pending()
            && observed.authorized
            && observed.current
            && matches!(attempt, Attempt::Applied(writes) if self.admissible(may_write, writes))
    }

    fn of_row(row: &OwedDeliveryRow) -> Self {
        Self {
            key: Key {
                point: row.point.clone(),
                plugin: row.plugin.clone(),
                subject: row.delivery_key.clone(),
            },
            kind: Kind::of_point(&row.point),
            actor: row.actor_did.clone(),
            runner: row.runner_did.clone(),
            attempts: u64::try_from(row.attempts).unwrap_or_default(),
            last_error: row.last_error.clone(),
            settled: match row.settlement {
                OwedSettlement::Pending => None,
                OwedSettlement::Delivered => Some(Settlement::Delivered),
                OwedSettlement::Dropped => Some(Settlement::Dropped),
            },
        }
    }
}

/// Lean `Hooks.OwedDelivery.publish`: a fresh record per owed key unless one
/// with that key exists.
pub fn publish<I, F>(records: &mut Vec<Record>, fresh: F, keys: I, commit: bool)
where
    I: IntoIterator<Item = Key>,
    F: Fn(Key) -> Record,
{
    if !commit {
        return;
    }
    for key in keys {
        if !records.iter().any(|record| record.key == key) {
            records.push(fresh(key));
        }
    }
}

/// Lean `Hooks.OwedDelivery.HaltedSession`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaltedSession {
    pub session: String,
    pub owner: Option<String>,
    pub latched_at: Option<String>,
    pub current: bool,
}

/// One halt re-derived from core state: (session, halt time, owning Agent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedHalt {
    pub session: String,
    pub at: String,
    pub owner: String,
}

/// Lean `Hooks.OwedDelivery.deriveHalts`.
pub fn derive_halts(bound: usize, sessions: &[HaltedSession]) -> Vec<DerivedHalt> {
    sessions
        .iter()
        .filter(|session| {
            session.owner.is_some() && session.latched_at.is_some() && session.current
        })
        .take(bound)
        .filter_map(|session| {
            Some(DerivedHalt {
                session: session.session.clone(),
                at: session.latched_at.clone()?,
                owner: session.owner.clone()?,
            })
        })
        .collect()
}

/// Lean `Hooks.OwedDelivery.prunable`: settled for longer than the retention.
pub fn prunable(record: &Record, age: u64, retention: u64) -> bool {
    record.settled.is_some() && retention < age
}

/// Whether a request terminal is still the one owed: the request is terminal
/// in the recorded state. A terminal is final, so nothing supersedes it.
pub fn request_current(recorded: Option<&str>, observed: Option<&str>) -> bool {
    recorded.is_some() && recorded == observed
}

/// Whether a halt is still the one owed: the session's stop latch is as the
/// halt left it (a resume clears it, a later stop replaces it) and no request
/// of the session was created after the halt.
pub fn halt_current(
    recorded_latch: Option<&str>,
    observed_latch: Option<&str>,
    newest_request_at: Option<&str>,
    occurred_at: &str,
) -> bool {
    recorded_latch == observed_latch && not_after(newest_request_at, occurred_at)
}

/// Whether `later`, if any, is at or before `at`; an unreadable time is
/// treated as after.
fn not_after(later: Option<&str>, at: &str) -> bool {
    let Some(later) = later else {
        return true;
    };
    match (
        chrono::DateTime::parse_from_rfc3339(later),
        chrono::DateTime::parse_from_rfc3339(at),
    ) {
        (Ok(later), Ok(at)) => later <= at,
        _ => false,
    }
}

/// The scalars of the event a dropped staging owes. `owner` is the Agent that
/// owns the subject session, under which the staging is redriven.
pub(super) struct Owing<'a> {
    pub point: &'static str,
    pub subject: OwedSubject,
    pub subject_id: &'a str,
    pub subject_node_did: &'a str,
    pub owner: Option<&'a str>,
    pub terminal_state: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub occurred_at: &'a str,
    pub halt_latch: Option<&'a str>,
}

/// Records every dropped staging as owed, published after `txn` commits
/// (Lean `Hooks.owed`, `Hooks.OwedDelivery.owe` and `publish`). The record
/// carries the transaction's actor, if any, for audit, and the identity its
/// redrive runs as.
pub(super) fn owe(txn: &ConfigApplyTxn<'_>, owing: &Owing<'_>, staged: &[Staged]) {
    let now = chrono::Utc::now().to_rfc3339();
    let kind = Kind::of_point(owing.point);
    let actor = txn.actor_did();
    for entry in staged {
        let Staged::Owed { plugin, cause } = entry else {
            continue;
        };
        tracing::warn!(
            point = owing.point,
            plugin,
            subject = owing.subject_id,
            error = %gents_protocol::error_chain::ErrorChain(cause),
            "lifecycle plugin staging was dropped; its delivery is owed"
        );
        // The bundled points that owe are all must-commit stagings, which no
        // actor's signing capability gates; an `OnBehalf` point passes the
        // runtime's signing check here when one is added.
        let Some(runner) = owe_runner(kind, actor, owing.owner, |_| false) else {
            match kind {
                Kind::Staging => tracing::warn!(
                    point = owing.point,
                    plugin,
                    subject = owing.subject_id,
                    "an owed lifecycle delivery's subject has no visible owning Agent; it is not recorded"
                ),
                Kind::OnBehalf => tracing::warn!(
                    point = owing.point,
                    plugin,
                    subject = owing.subject_id,
                    actor,
                    "an owed lifecycle delivery acts for an actor this runtime cannot sign as; it is refused"
                ),
            }
            continue;
        };
        let occurrence = match owing.subject {
            OwedSubject::Request => None,
            OwedSubject::Session => Some(owing.occurred_at),
        };
        txn.owe_delivery(OwedDeliveryRow {
            doc_id: None,
            delivery_key: delivery_key(
                owing.point,
                plugin,
                owing.subject,
                owing.subject_id,
                occurrence,
            ),
            node_did: owing.subject_node_did.to_owned(),
            node_doc_id: None,
            parent_linked: Some(false),
            point: owing.point.to_owned(),
            plugin: (*plugin).to_owned(),
            subject_kind: owing.subject,
            subject_id: owing.subject_id.to_owned(),
            subject_node_did: owing.subject_node_did.to_owned(),
            actor_did: actor.map(str::to_owned),
            runner_did: runner,
            terminal_state: owing.terminal_state.map(str::to_owned),
            reason: owing.reason.map(str::to_owned),
            occurred_at: owing.occurred_at.to_owned(),
            halt_latch: owing.halt_latch.map(str::to_owned),
            attempts: 0,
            last_error: None,
            settlement: OwedSettlement::Pending,
            created_at: now.clone(),
            updated_at: now.clone(),
        });
    }
}

/// The Agent that owns the session a request belongs to, as `txn` sees it.
pub(super) async fn request_session_owner(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> anyhow::Result<Option<SessionOwnerRow>> {
    let response = txn
        .run(
            &Query::select(
                Selection::doc_id()
                    .field(request_fields::SESSION_ID)
                    .field(request_fields::NODE_DID)
                    .field(request_fields::LIFECYCLE_STATE),
            )
            .filter(Filter::eq(request_fields::REQUEST_ID, request_id))
            .limit(2)
            .render(),
        )
        .await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "AgentRequest");
    let [request] = rows.as_slice() else {
        return Ok(None);
    };
    let (Some(session_id), Some(node_did)) =
        (request["session_id"].as_str(), request["node_did"].as_str())
    else {
        return Ok(None);
    };
    load_agent_session_owner_in_txn(txn, node_did, session_id).await
}

/// Why one redrive could not settle or count its record.
#[derive(Debug, thiserror::Error)]
pub enum RedriveError {
    #[error("owed delivery recovery requires the configured runtime identity")]
    RuntimeIdentity,
    #[error(transparent)]
    Store(#[from] gents_store::config_client::ConfigClientError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

const NO_SIGNER: &str = "this runtime holds no signing capability for the delivery's runner";

/// Halts re-derived per sweep, across the node's Agents (Lean
/// `deriveHalts`'s bound).
pub const DERIVED_HALTS_PER_SWEEP: usize = 16;

/// A latch older than this is not re-derived: the window it closes is a
/// process stopping between a commit and its record's publication, which the
/// first sweep after restart covers.
pub const DERIVED_HALT_HORIZON: chrono::Duration = chrono::Duration::hours(24);

/// Settled records older than this are deleted by the sweep, as the node key.
pub const SETTLED_RETENTION: chrono::Duration = chrono::Duration::days(7);

/// Settled records deleted per sweep.
const PRUNED_PER_SWEEP: u64 = 64;

/// The plugin at a point, if the process still registers it.
fn plugin<'r, E>(plugins: &'r [Plugin<E, ()>], name: &str) -> Option<&'r Plugin<E, ()>> {
    plugins.iter().find(|plugin| plugin.manifest().name == name)
}

fn unowned() -> Observation {
    Observation {
        authorized: false,
        current: false,
    }
}

/// What the runner observes of the subject request in `txn`, and the event
/// rebuilt from core's rows when the delivery is still owed. For a staging the
/// subject is owned when its session's Agent is the runner.
async fn observe_request(
    txn: &ConfigApplyTxn<'_>,
    row: &OwedDeliveryRow,
    kind: Kind,
) -> anyhow::Result<(Observation, Option<RequestTerminal>)> {
    let response = txn
        .run(
            &Query::select(
                Selection::doc_id()
                    .field(request_fields::LIFECYCLE_STATE)
                    .field(request_fields::SESSION_ID)
                    .field(request_fields::NODE_DID),
            )
            .filter(Filter::eq(
                request_fields::REQUEST_ID,
                row.subject_id.as_str(),
            ))
            .limit(2)
            .render(),
        )
        .await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "AgentRequest");
    let [request] = rows.as_slice() else {
        return Ok((unowned(), None));
    };
    if kind == Kind::Staging {
        let owned = match (request["session_id"].as_str(), request["node_did"].as_str()) {
            (Some(session_id), Some(node_did)) => {
                load_agent_session_owner_in_txn(txn, node_did, session_id)
                    .await?
                    .is_some_and(|session| session.session.agent_did == row.runner_did)
            }
            _ => false,
        };
        if !owned {
            return Ok((unowned(), None));
        }
    }
    let current = request_current(
        row.terminal_state.as_deref(),
        request["lifecycle_state"].as_str(),
    );
    let event = match (current, row.terminal_state.as_deref()) {
        (true, Some(state)) => Some(RequestTerminal {
            node_did: row.subject_node_did.clone(),
            request_id: row.subject_id.clone(),
            state: RequestLifecycleState::parse(state)?,
            reason: row.reason.clone().unwrap_or_default(),
            at: row.occurred_at.clone(),
        }),
        _ => None,
    };
    Ok((
        Observation {
            authorized: true,
            current,
        },
        event,
    ))
}

/// The newest request of a session as `txn` sees it: its creation time and
/// interrupt latch.
async fn newest_request(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> anyhow::Result<Option<(Option<String>, Option<String>)>> {
    let response = txn
        .run(
            &Query::select(
                Selection::doc_id()
                    .field(request_fields::CREATED_AT)
                    .field(request_fields::INTERRUPT_REQUESTED_AT),
            )
            .filter(Filter::eq(request_fields::SESSION_ID, session_id))
            .filter(Filter::eq(request_fields::NODE_DID, node_did))
            .order_by(request_fields::CREATED_AT, Direction::Desc)
            .limit(1)
            .render(),
        )
        .await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "AgentRequest");
    Ok(rows.first().map(|request| {
        (
            request["created_at"].as_str().map(str::to_owned),
            request["interrupt_requested_at"]
                .as_str()
                .filter(|latch| !latch.is_empty())
                .map(str::to_owned),
        )
    }))
}

async fn observe_session(
    txn: &ConfigApplyTxn<'_>,
    row: &OwedDeliveryRow,
    kind: Kind,
) -> anyhow::Result<(Observation, Option<SessionHalt>)> {
    let Some(session) =
        load_agent_session_owner_in_txn(txn, &row.subject_node_did, &row.subject_id).await?
    else {
        return Ok((unowned(), None));
    };
    if kind == Kind::Staging && session.session.agent_did != row.runner_did {
        return Ok((unowned(), None));
    }
    let newest = newest_request(txn, &row.subject_node_did, &row.subject_id).await?;
    let current = halt_current(
        row.halt_latch.as_deref(),
        session.session.stopped_at.as_deref(),
        newest.as_ref().and_then(|(created, _)| created.as_deref()),
        &row.occurred_at,
    );
    let event = current.then(|| SessionHalt {
        node_did: row.subject_node_did.clone(),
        session_id: row.subject_id.clone(),
        at: row.occurred_at.clone(),
    });
    Ok((
        Observation {
            authorized: true,
            current,
        },
        event,
    ))
}

/// Runs one plugin's hook as a refusable `stage` for just that plugin.
async fn stage_one<E>(
    txn: &ConfigApplyTxn<'_>,
    registry: &Registry,
    plugin: &Plugin<E, ()>,
    event: &E,
) -> Result<(), HookError> {
    let host = TxnHost::new(txn, registry, OperationContext::default());
    stage(&host, std::slice::from_ref(plugin), event).await?;
    Ok(())
}

/// One redrive in the runner's own transaction: observe the subject under the
/// runner's current native authority and, when still owed, run the plugin's
/// hook. Native ACP for the runner admits each write as it executes, so a
/// committed staging made only admissible writes; `Ok` carries the
/// observation of a transaction whose staging committed, or that ran no hook,
/// and `Err` is a failed attempt.
async fn attempt(
    access: &ConfigAccess,
    registry: &Registry,
    row: &OwedDeliveryRow,
) -> anyhow::Result<Observation> {
    let kind = Kind::of_point(&row.point);
    access
        .transact_write("hooks.owed_delivery.redrive", |txn| {
            Box::pin(async move {
                match row.subject_kind {
                    OwedSubject::Request => {
                        let (observed, event) = observe_request(txn, row, kind).await?;
                        match (event, plugin(&registry.request_terminal, &row.plugin)) {
                            (Some(event), Some(plugin)) => {
                                stage_one(txn, registry, plugin, &event).await?;
                                Ok(observed)
                            }
                            (_, None) => Ok(Observation {
                                current: false,
                                ..observed
                            }),
                            (None, Some(_)) => Ok(observed),
                        }
                    }
                    OwedSubject::Session => {
                        let (observed, event) = observe_session(txn, row, kind).await?;
                        match (event, plugin(&registry.session_halt, &row.plugin)) {
                            (Some(event), Some(plugin)) => {
                                stage_one(txn, registry, plugin, &event).await?;
                                Ok(observed)
                            }
                            (_, None) => Ok(Observation {
                                current: false,
                                ..observed
                            }),
                            (None, Some(_)) => Ok(observed),
                        }
                    }
                }
            })
        })
        .await
}

/// The runner's access, if this runtime can sign as it. A delivery runs as its
/// runner and never as another identity (Lean `Hooks.OwedDelivery.owe`).
fn runner_access(
    node: &Arc<EmbeddedNode>,
    identities: &[Arc<dyn NodeIdentity>],
    runner: &str,
) -> anyhow::Result<Option<ConfigAccess>> {
    let Some(identity) = identities.iter().find(|identity| identity.did() == runner) else {
        return Ok(None);
    };
    Ok(Some(ConfigAccess::local_managed_agent(
        Arc::clone(node),
        identity::Did::new(runner)?,
        identity.as_ref(),
    )?))
}

/// Writes a redrive's result as the runtime, only while the record is still
/// pending, so concurrent sweeps settle it once.
async fn settle(
    runtime_access: &ConfigAccess,
    row: &OwedDeliveryRow,
    doc_id: &str,
    next: &Record,
) -> anyhow::Result<()> {
    let settlement = match next.settled {
        None => OwedSettlement::Pending,
        Some(Settlement::Delivered) => OwedSettlement::Delivered,
        Some(Settlement::Dropped) => OwedSettlement::Dropped,
    };
    let rendered = Update::doc(
        doc_id,
        Input::new()
            .set(fields::SETTLEMENT, settlement.as_str())
            .set(
                fields::ATTEMPTS,
                i64::try_from(next.attempts).unwrap_or(i64::MAX),
            )
            .set_optional(fields::LAST_ERROR, next.last_error.as_deref())
            .set(fields::UPDATED_AT, chrono::Utc::now().to_rfc3339()),
    )
    .filter(Filter::eq(
        fields::SETTLEMENT,
        OwedSettlement::Pending.as_str(),
    ))
    .render();
    runtime_access
        .write_rendered("hooks.owed_delivery.settle", &rendered)
        .await?;
    tracing::debug!(
        point = row.point,
        plugin = row.plugin,
        subject = row.subject_id,
        settlement = settlement.as_str(),
        "owed lifecycle delivery redriven"
    );
    Ok(())
}

fn failed(error: &anyhow::Error) -> (Observation, Attempt) {
    (
        Observation {
            authorized: true,
            current: true,
        },
        Attempt::Failed(format!(
            "{}",
            gents_protocol::error_chain::ErrorChain(error.as_ref())
        )),
    )
}

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwedSweepReport {
    pub settled: usize,
    pub derived_halts: usize,
    pub pruned: usize,
}

impl OwedSweepReport {
    pub fn is_noop(&self) -> bool {
        self.settled == 0 && self.derived_halts == 0 && self.pruned == 0
    }
}

/// The periodic sweep (Lean `Recovery.owedDeliveryRecoverySweep`): redrives
/// pending owed deliveries, re-derives owed halts from core state, and prunes
/// settled records past the retention. Only a record whose native owner is the
/// node key is redriven or pruned (`Identity.OwedDeliveryAccess.runtimeOwned`).
pub async fn redrive_pending(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[Arc<dyn NodeIdentity>],
) -> Result<OwedSweepReport, RedriveError> {
    if node.node_identity_did() != Some(node_did) {
        return Err(RedriveError::RuntimeIdentity);
    }
    let runtime = identity::Did::new(node_did).map_err(|_| RedriveError::RuntimeIdentity)?;
    let runtime_access = ConfigAccess::local_as(Arc::clone(node), runtime.clone());
    let registry = Registry::for_node(node, "owed.redrive").map_err(anyhow::Error::from)?;
    let settled = redrive_records(node, &runtime, &runtime_access, &registry, identities).await?;
    let derived_halts = derive_owed_halts(node, node_did, &registry, identities).await;
    let pruned = prune_settled(&runtime, &runtime_access).await?;
    Ok(OwedSweepReport {
        settled,
        derived_halts,
        pruned,
    })
}

/// Rows of `query` whose native owner is the runtime.
async fn runtime_owned_rows(
    runtime: &identity::Did,
    runtime_access: &ConfigAccess,
    query: &gents_protocol::document_query::Rendered,
) -> Result<Vec<OwedDeliveryRow>, RedriveError> {
    let (rows, owners) = runtime_access
        .transact_readonly("hooks.owed_delivery.read", |txn| {
            Box::pin(async move {
                let response = txn.run(query).await?;
                let rows: Vec<OwedDeliveryRow> =
                    gents_protocol::graphql::graphql_rows_from_response(&response, "OwedDelivery")
                        .into_iter()
                        .map(serde_json::from_value)
                        .collect::<Result<_, _>>()?;
                let ids: Vec<String> = rows.iter().filter_map(|row| row.doc_id.clone()).collect();
                let owners = if ids.is_empty() {
                    Vec::new()
                } else {
                    txn.document_owners("OwedDelivery", &ids).await?
                };
                Ok::<_, anyhow::Error>((rows, owners))
            })
        })
        .await?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            owners.iter().any(|owner| {
                Some(owner.doc_id.as_str()) == row.doc_id.as_deref()
                    && owner.visible
                    && owner.owner.as_ref() == Some(runtime)
            })
        })
        .collect())
}

async fn redrive_records(
    node: &Arc<EmbeddedNode>,
    runtime: &identity::Did,
    runtime_access: &ConfigAccess,
    registry: &Registry,
    identities: &[Arc<dyn NodeIdentity>],
) -> Result<usize, RedriveError> {
    let pending = Query::select(Selection::<fields::Doc>::owned(OWED_DELIVERY_FIELDS))
        .filter(Filter::eq(fields::NODE_DID, runtime.as_str()))
        .filter(Filter::eq(
            fields::SETTLEMENT,
            OwedSettlement::Pending.as_str(),
        ))
        .render();
    let rows = runtime_owned_rows(runtime, runtime_access, &pending).await?;
    let mut settled = 0;
    for row in &rows {
        let Some(doc_id) = row.doc_id.as_deref() else {
            continue;
        };
        let record = Record::of_row(row);
        let (observed, outcome) = match runner_access(node, identities, &row.runner_did) {
            Ok(Some(access)) => match attempt(&access, registry, row).await {
                Ok(observed) => (observed, Attempt::Applied(Vec::new())),
                Err(error) => failed(&error),
            },
            Ok(None) => (
                Observation {
                    authorized: true,
                    current: true,
                },
                Attempt::Failed(NO_SIGNER.to_owned()),
            ),
            Err(error) => failed(&error),
        };
        let next = record.redrive(observed, &outcome, |_| true);
        if let Err(error) = settle(runtime_access, row, doc_id, &next).await {
            tracing::warn!(
                point = row.point,
                plugin = row.plugin,
                subject = row.subject_id,
                error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
                "owed lifecycle delivery result was not recorded; the next sweep redrives it"
            );
            continue;
        }
        settled += usize::from(!next.pending());
    }
    Ok(settled)
}

/// The sessions one Agent owns whose halt latch is set within the horizon,
/// with whether that halt is still current.
async fn halted_sessions(
    access: &ConfigAccess,
    node_did: &str,
    agent: &str,
) -> anyhow::Result<Vec<(HaltedSession, String)>> {
    let horizon = (chrono::Utc::now() - DERIVED_HALT_HORIZON).to_rfc3339();
    let bound = u64::try_from(DERIVED_HALTS_PER_SWEEP).unwrap_or(u64::MAX);
    access
        .transact_readonly("hooks.owed_delivery.halted_sessions", |txn| {
            let horizon = horizon.as_str();
            Box::pin(async move {
                let stopped = txn
                    .run(
                        &Query::select(Selection::doc_id().field(session_fields::SESSION_ID))
                            .filter(Filter::eq(session_fields::NODE_DID, node_did))
                            .filter(Filter::eq(session_fields::AGENT_DID, agent))
                            .filter(Filter::not_null(session_fields::STOPPED_AT))
                            .order_by(session_fields::STOPPED_AT, Direction::Desc)
                            .limit(bound)
                            .render(),
                    )
                    .await?;
                let interrupted = txn
                    .run(
                        &Query::select(Selection::doc_id().field(request_fields::SESSION_ID))
                            .filter(Filter::eq(request_fields::NODE_DID, node_did))
                            .filter(Filter::eq(request_fields::AGENT_DID, agent))
                            .filter(Filter::not_null(request_fields::INTERRUPT_REQUESTED_AT))
                            .order_by(request_fields::INTERRUPT_REQUESTED_AT, Direction::Desc)
                            .limit(bound)
                            .render(),
                    )
                    .await?;
                let mut candidates: Vec<String> = Vec::new();
                for row in
                    gents_protocol::graphql::graphql_rows_from_response(&stopped, "AgentSession")
                        .into_iter()
                        .chain(gents_protocol::graphql::graphql_rows_from_response(
                            &interrupted,
                            "AgentRequest",
                        ))
                {
                    if let Some(session_id) = row["session_id"].as_str() {
                        if !candidates.iter().any(|known| known == session_id) {
                            candidates.push(session_id.to_owned());
                        }
                    }
                }
                let mut sessions = Vec::with_capacity(candidates.len());
                for session_id in candidates {
                    let Some(session) =
                        load_agent_session_owner_in_txn(txn, node_did, &session_id).await?
                    else {
                        continue;
                    };
                    let newest = newest_request(txn, node_did, &session_id).await?;
                    let (newest_created, newest_interrupt) = newest.unwrap_or((None, None));
                    // The stop latch is current while no request followed it;
                    // otherwise the newest request's interrupt latch, which no
                    // later request can have followed.
                    let latch = match session.session.stopped_at.clone() {
                        Some(stopped) if not_after(newest_created.as_deref(), &stopped) => {
                            Some(stopped)
                        }
                        _ => newest_interrupt,
                    };
                    let latch = latch.filter(|latch| latch.as_str() >= horizon);
                    sessions.push((
                        HaltedSession {
                            session: session_id,
                            owner: (session.session.agent_did == agent).then(|| agent.to_owned()),
                            current: latch.is_some(),
                            latched_at: latch,
                        },
                        session.session.node_did.clone(),
                    ));
                }
                Ok::<_, anyhow::Error>(sessions)
            })
        })
        .await
}

/// Re-derives owed halts from durable core state, closing the publication
/// window (Lean `Hooks.OwedDelivery.deriveHalts`): for each session an Agent
/// owns whose stop or interrupt latch is set and still current, the halt
/// point's listeners run as a redrive under that Agent. This covers a halt
/// whose owing transaction ran in a client process over GraphQL, which records
/// nothing. The listeners are idempotent: the Goals listener stages nothing
/// for a Goal that is not active or that changed after the halt.
async fn derive_owed_halts(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    registry: &Registry,
    identities: &[Arc<dyn NodeIdentity>],
) -> usize {
    if registry.session_halt.is_empty() {
        return 0;
    }
    let mut sessions = Vec::new();
    let mut accesses = Vec::new();
    for identity in identities {
        let agent = identity.did();
        if agent == node_did {
            continue;
        }
        let access = match runner_access(node, identities, agent) {
            Ok(Some(access)) => access,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(agent_did = agent, error = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "owed halt derivation signing capability unavailable");
                continue;
            }
        };
        match halted_sessions(&access, node_did, agent).await {
            Ok(found) => sessions.extend(found),
            Err(error) => {
                tracing::warn!(agent_did = agent, error = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "owed halt derivation could not read halted sessions")
            }
        }
        accesses.push((agent.to_owned(), access));
    }
    let halted: Vec<HaltedSession> = sessions
        .iter()
        .map(|(session, _)| session.clone())
        .collect();
    let mut derived = 0;
    for halt in derive_halts(DERIVED_HALTS_PER_SWEEP, &halted) {
        let Some((_, access)) = accesses.iter().find(|(agent, _)| *agent == halt.owner) else {
            continue;
        };
        let Some(session_node) = sessions
            .iter()
            .find(|(session, _)| session.session == halt.session)
            .map(|(_, node)| node.clone())
        else {
            continue;
        };
        let event = SessionHalt {
            node_did: session_node,
            session_id: halt.session.clone(),
            at: halt.at.clone(),
        };
        for plugin in &registry.session_halt {
            let result = access
                .transact_write("hooks.owed_delivery.derived_halt", |txn| {
                    let event = &event;
                    Box::pin(async move {
                        stage_one(txn, registry, plugin, event)
                            .await
                            .map_err(anyhow::Error::from)
                    })
                })
                .await;
            match result {
                Ok(()) => derived += 1,
                Err(error) => tracing::warn!(
                    plugin = plugin.manifest().name,
                    session = halt.session,
                    error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
                    "a re-derived halt was not delivered; the next sweep derives it again"
                ),
            }
        }
    }
    derived
}

/// Deletes, as the node key, settled records older than the retention (Lean
/// `Hooks.OwedDelivery.prune`); pending records are never deleted.
async fn prune_settled(
    runtime: &identity::Did,
    runtime_access: &ConfigAccess,
) -> Result<usize, RedriveError> {
    let now = chrono::Utc::now();
    let cutoff = (now - SETTLED_RETENTION).to_rfc3339();
    let settled = Query::select(Selection::<fields::Doc>::owned(OWED_DELIVERY_FIELDS))
        .filter(Filter::eq(fields::NODE_DID, runtime.as_str()))
        .filter(Filter::ne(
            fields::SETTLEMENT,
            OwedSettlement::Pending.as_str(),
        ))
        .filter(Filter::lt(fields::UPDATED_AT, cutoff.as_str()))
        .limit(PRUNED_PER_SWEEP)
        .render();
    let retention = u64::try_from(SETTLED_RETENTION.num_seconds()).unwrap_or(u64::MAX);
    let mut pruned = 0;
    for row in runtime_owned_rows(runtime, runtime_access, &settled).await? {
        let (Some(doc_id), Ok(updated)) = (
            row.doc_id.as_deref(),
            chrono::DateTime::parse_from_rfc3339(&row.updated_at),
        ) else {
            continue;
        };
        let age = u64::try_from((now - updated.with_timezone(&chrono::Utc)).num_seconds())
            .unwrap_or_default();
        if !prunable(&Record::of_row(&row), age, retention) {
            continue;
        }
        let deleted = runtime_access
            .write_rendered(
                "hooks.owed_delivery.prune",
                &Delete::<fields::Doc>::doc(doc_id)
                    .filter(Filter::ne(
                        fields::SETTLEMENT,
                        OwedSettlement::Pending.as_str(),
                    ))
                    .render(),
            )
            .await;
        match deleted {
            Ok(_) => pruned += 1,
            Err(error) => tracing::warn!(
                subject = row.subject_id,
                error = %gents_protocol::error_chain::ErrorChain(&error),
                "a settled owed delivery was not pruned; the next sweep prunes it"
            ),
        }
    }
    Ok(pruned)
}
