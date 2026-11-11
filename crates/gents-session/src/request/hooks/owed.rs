//! Owed lifecycle deliveries (Lean `Hooks.OwedDelivery`): the record a
//! must-commit point publishes for a plugin whose staging was dropped, and the
//! redrive that runs that plugin's hook later, as the recorded actor.

use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::document_query::{
    fields::agent_request as request_fields, fields::owed_delivery as fields, Direction, Filter,
    Input, Query, Selection, Update,
};
use gents_protocol::owed_delivery::{
    delivery_key, OwedDeliveryRow, OwedSettlement, OwedSubject, OWED_DELIVERY_FIELDS,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
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
    pub actor: String,
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

/// Lean `Hooks.OwedDelivery.Attempt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    Applied,
    Failed(String),
}

impl Record {
    /// Lean `Hooks.OwedDelivery.Record.fresh`.
    pub fn fresh(actor: &str, key: Key) -> Self {
        Self {
            key,
            actor: actor.to_owned(),
            attempts: 0,
            last_error: None,
            settled: None,
        }
    }

    pub fn pending(&self) -> bool {
        self.settled.is_none()
    }

    /// Lean `Hooks.OwedDelivery.redrive`.
    pub fn redrive(&self, observed: Observation, attempt: &Attempt) -> Self {
        if self.settled.is_some() {
            return self.clone();
        }
        if !observed.authorized || !observed.current {
            return Self {
                settled: Some(Settlement::Dropped),
                ..self.clone()
            };
        }
        match attempt {
            Attempt::Applied => Self {
                settled: Some(Settlement::Delivered),
                ..self.clone()
            },
            Attempt::Failed(error) => Self {
                attempts: self.attempts + 1,
                last_error: Some(error.clone()),
                ..self.clone()
            },
        }
    }

    /// Lean `Hooks.OwedDelivery.performs`.
    pub fn performs(&self, observed: Observation, attempt: &Attempt) -> bool {
        self.pending() && observed.authorized && observed.current && *attempt == Attempt::Applied
    }

    fn of_row(row: &OwedDeliveryRow) -> Self {
        Self {
            key: Key {
                point: row.point.clone(),
                plugin: row.plugin.clone(),
                subject: row.delivery_key.clone(),
            },
            actor: row.actor_did.clone(),
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
pub fn publish<I>(records: &mut Vec<Record>, actor: &str, keys: I, commit: bool)
where
    I: IntoIterator<Item = Key>,
{
    if !commit {
        return;
    }
    for key in keys {
        if !records.iter().any(|record| record.key == key) {
            records.push(Record::fresh(actor, key));
        }
    }
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
    if recorded_latch != observed_latch {
        return false;
    }
    let Some(newest) = newest_request_at else {
        return true;
    };
    match (
        chrono::DateTime::parse_from_rfc3339(newest),
        chrono::DateTime::parse_from_rfc3339(occurred_at),
    ) {
        (Ok(newest), Ok(occurred)) => newest <= occurred,
        _ => false,
    }
}

/// The scalars of the event a dropped staging owes.
pub(super) struct Owing<'a> {
    pub point: &'static str,
    pub subject: OwedSubject,
    pub subject_id: &'a str,
    pub subject_node_did: &'a str,
    pub terminal_state: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub occurred_at: &'a str,
    pub halt_latch: Option<&'a str>,
}

/// Records every dropped staging as owed, published after `txn` commits
/// (Lean `Hooks.owed`, `Hooks.OwedDelivery.publish`). The record carries the
/// transaction's actor, under which the delivery later runs.
pub(super) fn owe(txn: &ConfigApplyTxn<'_>, owing: &Owing<'_>, staged: &[Staged]) {
    let now = chrono::Utc::now().to_rfc3339();
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
        let Some(actor) = txn.actor_did() else {
            tracing::warn!(
                point = owing.point,
                plugin,
                subject = owing.subject_id,
                "an owed lifecycle delivery has no actor to run under and is not recorded"
            );
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
            actor_did: actor.to_owned(),
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

/// Why one redrive could not settle or count its record.
#[derive(Debug, thiserror::Error)]
pub enum RedriveError {
    #[error("owed delivery recovery requires the configured runtime identity")]
    RuntimeIdentity,
    #[error("owed delivery has an unreadable terminal state")]
    TerminalState(#[source] gents_protocol::request_lifecycle::InvalidRequestLifecycleState),
    #[error(transparent)]
    Store(#[from] gents_store::config_client::ConfigClientError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

const NO_SIGNER: &str = "this runtime holds no signing capability for the recorded actor";

/// The plugin at a point, if the process still registers it.
fn plugin<'r, E>(plugins: &'r [Plugin<E, ()>], name: &str) -> Option<&'r Plugin<E, ()>> {
    plugins.iter().find(|plugin| plugin.manifest().name == name)
}

/// What the actor observes of the subject in `txn`, and the event rebuilt
/// from core's rows when the delivery is still owed.
async fn observe_request(
    txn: &ConfigApplyTxn<'_>,
    row: &OwedDeliveryRow,
) -> anyhow::Result<(Observation, Option<RequestTerminal>)> {
    let response = txn
        .run(
            &Query::select(Selection::doc_id().field(request_fields::LIFECYCLE_STATE))
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
        return Ok((
            Observation {
                authorized: false,
                current: false,
            },
            None,
        ));
    };
    let observed = request["lifecycle_state"].as_str();
    let current = request_current(row.terminal_state.as_deref(), observed);
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

async fn observe_session(
    txn: &ConfigApplyTxn<'_>,
    row: &OwedDeliveryRow,
) -> anyhow::Result<(Observation, Option<SessionHalt>)> {
    let Some(session) = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &row.subject_node_did,
        &row.subject_id,
    )
    .await?
    else {
        return Ok((
            Observation {
                authorized: false,
                current: false,
            },
            None,
        ));
    };
    let response = txn
        .run(
            &Query::select(Selection::doc_id().field(request_fields::CREATED_AT))
                .filter(Filter::eq(
                    request_fields::SESSION_ID,
                    row.subject_id.as_str(),
                ))
                .filter(Filter::eq(
                    request_fields::NODE_DID,
                    row.subject_node_did.as_str(),
                ))
                .order_by(request_fields::CREATED_AT, Direction::Desc)
                .limit(1)
                .render(),
        )
        .await?;
    let newest = gents_protocol::graphql::graphql_rows_from_response(&response, "AgentRequest");
    let current = halt_current(
        row.halt_latch.as_deref(),
        session.session.stopped_at.as_deref(),
        newest
            .first()
            .and_then(|request| request["created_at"].as_str()),
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

/// One redrive in the recorded actor's own transaction: observe the subject
/// under that actor's current native authority and, when still owed, run the
/// plugin's hook. `Ok` carries the observation of a transaction whose staging
/// committed, or that ran no hook; `Err` is a failed attempt.
async fn attempt(
    access: &ConfigAccess,
    registry: &Registry,
    row: &OwedDeliveryRow,
) -> anyhow::Result<Observation> {
    access
        .transact_write("hooks.owed_delivery.redrive", |txn| {
            Box::pin(async move {
                match row.subject_kind {
                    OwedSubject::Request => {
                        let (observed, event) = observe_request(txn, row).await?;
                        let plugin = plugin(&registry.request_terminal, &row.plugin);
                        match (event, plugin) {
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
                        let (observed, event) = observe_session(txn, row).await?;
                        let plugin = plugin(&registry.session_halt, &row.plugin);
                        match (event, plugin) {
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

/// The recorded actor's access, if this runtime can act as it. A delivery
/// runs as that actor and never as another (Lean `Hooks.Safety.perform`).
fn actor_access(
    node: &Arc<EmbeddedNode>,
    runtime: &identity::Did,
    identities: &[Arc<dyn NodeIdentity>],
    actor: &str,
) -> anyhow::Result<Option<ConfigAccess>> {
    if actor == runtime.as_str() {
        return Ok(Some(ConfigAccess::local_as(
            Arc::clone(node),
            runtime.clone(),
        )));
    }
    let Some(identity) = identities.iter().find(|identity| identity.did() == actor) else {
        return Ok(None);
    };
    Ok(Some(ConfigAccess::local_managed_agent(
        Arc::clone(node),
        identity::Did::new(actor)?,
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

/// The periodic sweep over pending owed deliveries (Lean
/// `Recovery.owedDeliveryRecoverySweep`). Only a record whose native owner is
/// the node key is redriven (`Identity.OwedDeliveryAccess.runtimeOwned`).
/// Returns how many records settled.
pub async fn redrive_pending(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[Arc<dyn NodeIdentity>],
) -> Result<usize, RedriveError> {
    if node.node_identity_did() != Some(node_did) {
        return Err(RedriveError::RuntimeIdentity);
    }
    let runtime = identity::Did::new(node_did).map_err(|_| RedriveError::RuntimeIdentity)?;
    let runtime_access = ConfigAccess::local_as(Arc::clone(node), runtime.clone());
    let pending = Query::select(Selection::<fields::Doc>::owned(OWED_DELIVERY_FIELDS))
        .filter(Filter::eq(fields::NODE_DID, node_did))
        .filter(Filter::eq(
            fields::SETTLEMENT,
            OwedSettlement::Pending.as_str(),
        ))
        .render();
    let (rows, owners) = runtime_access
        .transact_readonly("hooks.owed_delivery.pending", |txn| {
            let pending = &pending;
            Box::pin(async move {
                let response = txn.run(pending).await?;
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
    if rows.is_empty() {
        return Ok(0);
    }
    let registry = Registry::for_node(node).map_err(anyhow::Error::from)?;
    let mut settled = 0;
    for row in &rows {
        let Some(doc_id) = row.doc_id.as_deref() else {
            continue;
        };
        let runtime_owned = owners.iter().any(|owner| {
            owner.doc_id == doc_id && owner.visible && owner.owner.as_ref() == Some(&runtime)
        });
        if !runtime_owned {
            continue;
        }
        let record = Record::of_row(row);
        let (observed, outcome) = match actor_access(node, &runtime, identities, &row.actor_did) {
            Ok(Some(access)) => match attempt(&access, &registry, row).await {
                Ok(observed) => (observed, Attempt::Applied),
                Err(error) => (
                    Observation {
                        authorized: true,
                        current: true,
                    },
                    Attempt::Failed(format!(
                        "{}",
                        gents_protocol::error_chain::ErrorChain(error.as_ref())
                    )),
                ),
            },
            Ok(None) => (
                Observation {
                    authorized: true,
                    current: true,
                },
                Attempt::Failed(NO_SIGNER.to_owned()),
            ),
            Err(error) => (
                Observation {
                    authorized: true,
                    current: true,
                },
                Attempt::Failed(format!(
                    "{}",
                    gents_protocol::error_chain::ErrorChain(error.as_ref())
                )),
            ),
        };
        let next = record.redrive(observed, &outcome);
        if let Err(error) = settle(&runtime_access, row, doc_id, &next).await {
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
