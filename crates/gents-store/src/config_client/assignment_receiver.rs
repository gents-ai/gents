//! Consume issuer-scoped assignments from an embedded Vera home.
//!
//! For an inventory, call [`ConfigAccess::certified_assignments`] as the
//! receiving node. Supply its DID as the subject and select the issuer, policy,
//! resource and relation. The IDs and certified position come from the same
//! read. A successful empty result proves withdrawal for that scope. A failed
//! read leaves the last accepted position in effect.
//!
//! For a running workload, use [`ConfigAccess::watch_assignments`] with the
//! collection IDs that carry its bodies and an [`AssignmentRuntime`] that
//! can enumerate and stop work already running after a process restart.
//! If the issuer replaces B with C before C arrives locally, the receiver
//! records the new read floor, stops B, persists the applied set, and waits for C's verified body before
//! starting it. Recovery reads use the stored position as their lower bound.
//! [`ConfigAccess::load_assignments`] is for inspecting the last accepted
//! view. Call [`ConfigAccess::refresh_assignments`] for a one-shot reconciliation.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::future::Future;
use std::time::Duration;

use super::{
    assignments::AssignmentReadError, ConfigAccess, ConfigApplyTxn, ConfigClientError,
    TransactionFailure,
};

const RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct AssignmentScope {
    pub policy: String,
    pub resource: String,
    pub relation: String,
    pub issuer: String,
    pub node_document: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentState {
    pub position: defra_node::AssignmentPosition,
    pub assigned: Vec<String>,
}

impl AssignmentState {
    /// This reflects the last committed view; starting work requires a fresh refresh.
    pub fn may_run(&self, id: &str, body_verified: bool) -> bool {
        permits(
            self.assigned.iter().any(|assigned| assigned == id),
            body_verified,
        )
    }
}

fn permits(assigned: bool, body_verified: bool) -> bool {
    assigned && body_verified
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiverError {
    #[error("assignment receiver requires an explicit node identity")]
    MissingIdentity,
    #[error("invalid assignment state: {0}")]
    InvalidState(&'static str),
    #[error("assignment state was overtaken")]
    Overtaken,
    #[error(transparent)]
    Provider(#[from] AssignmentReadError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Store(#[from] ConfigClientError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Response(#[from] crate::graphql::GraphqlResponseError),
    #[error("assignment runtime failed")]
    Runtime(#[source] Box<dyn Error + Send + Sync>),
    #[error("assignment change subscription ended")]
    SubscriptionEnded,
}

#[async_trait::async_trait]
/// One assignment scope. `active` must include all work running in the scope,
/// even when another adapter started it. Implementations verify the body and
/// issuer signatures before reporting it available, make start and stop safe to
/// repeat, and leave no delayed effects after a call returns or is cancelled.
/// Starting must bind the verified body and signatures to the work started.
pub trait AssignmentRuntime: Send + Sync {
    async fn active(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>>;
    async fn stop(&self, id: &str) -> Result<(), Box<dyn Error + Send + Sync>>;
    async fn has_verified_body(&self, id: &str) -> Result<bool, Box<dyn Error + Send + Sync>>;
    async fn start_if_verified(&self, id: &str) -> Result<bool, Box<dyn Error + Send + Sync>>;
}

impl TransactionFailure for ReceiverError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Store(error) => Some(error),
            Self::Response(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl ReceiverError {
    fn retryable(&self) -> bool {
        let unavailable = matches!(
            self,
            Self::Store(ConfigClientError::DecisionReadUnanswered)
                | Self::Provider(AssignmentReadError::Provider(defra_node::Error::DacPolicy(
                    db::DacPolicyError::Assignments(acp::AssignmentReadError::Unavailable(_)),
                )))
        );
        retry(unavailable, matches!(self, Self::Overtaken))
    }
}

fn retry(unavailable: bool, overtaken: bool) -> bool {
    unavailable || overtaken
}

fn scope_key(subject: &str, scope: &AssignmentScope) -> String {
    let mut hash = blake3::Hasher::new();
    for value in [
        subject,
        &scope.policy,
        &scope.resource,
        &scope.relation,
        &scope.issuer,
    ] {
        hash.update(&(value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

struct Stored {
    document: String,
    state: AssignmentState,
}

async fn load(txn: &ConfigApplyTxn<'_>, key: &str) -> Result<Option<Stored>, ReceiverError> {
    let query = format!(
        "{{ CertifiedAssignmentState(filter: {{scope_key: {{_eq: \"{}\"}}}}, limit: 2) {{ _docID term height block_hash assigned }} }}",
        crate::graphql::escape_graphql_string(key)
    );
    let response = txn.execute_for_decision(&query, &json!({})).await?;
    let rows = response
        .pointer("/data/CertifiedAssignmentState")
        .and_then(Value::as_array)
        .ok_or(ReceiverError::InvalidState(
            "state query did not return rows",
        ))?;
    if rows.is_empty() {
        return Ok(None);
    }
    let [row] = rows.as_slice() else {
        return Err(ReceiverError::InvalidState(
            "unexpected row count or conflicting revision",
        ));
    };
    let string = |name: &str| {
        row.get(name)
            .and_then(Value::as_str)
            .ok_or(ReceiverError::InvalidState("invalid field"))
    };
    let term = string("term")?
        .parse()
        .map_err(|_| ReceiverError::InvalidState("missing field"))?;
    let height = string("height")?
        .parse()
        .map_err(|_| ReceiverError::InvalidState("missing field"))?;
    let assigned = serde_json::from_value(
        row.get("assigned")
            .cloned()
            .ok_or(ReceiverError::InvalidState("missing field"))?,
    )
    .map_err(|_| ReceiverError::InvalidState("invalid assignments"))?;
    Ok(Some(Stored {
        document: string("_docID")?.to_owned(),
        state: AssignmentState {
            position: defra_node::AssignmentPosition {
                term,
                height,
                block_hash: string("block_hash")?.to_owned(),
            },
            assigned,
        },
    }))
}

fn check(previous: &AssignmentState, next: &AssignmentState) -> Result<(), ReceiverError> {
    let old = (previous.position.term, previous.position.height);
    let new = (next.position.term, next.position.height);
    if new < old {
        return Err(ReceiverError::Overtaken);
    }
    if new == old
        && (next.position.block_hash != previous.position.block_hash
            || next.assigned != previous.assigned)
    {
        return Err(ReceiverError::InvalidState(
            "unexpected row count or conflicting revision",
        ));
    }
    Ok(())
}

async fn persist(
    txn: &ConfigApplyTxn<'_>,
    key: &str,
    subject: &str,
    scope: &AssignmentScope,
    next: &AssignmentState,
) -> Result<AssignmentState, ReceiverError> {
    let previous = load(txn, key).await?;
    if let Some(previous) = &previous {
        check(&previous.state, next)?;
        if previous.state == *next {
            return Ok(previous.state.clone());
        }
    }
    let update = json!({
        "term": next.position.term.to_string(),
        "height": next.position.height.to_string(),
        "block_hash": next.position.block_hash,
        "assigned": next.assigned,
    });
    match previous {
        Some(previous) => update_state(txn, &previous.document, update).await?,
        None => create_state(txn, key, subject, scope, update).await?,
    }
    Ok(next.clone())
}

async fn update_state(
    txn: &ConfigApplyTxn<'_>,
    id: &str,
    input: Value,
) -> Result<(), ReceiverError> {
    let response = txn.execute_with_variables(
        "mutation($id: ID!, $input: CertifiedAssignmentStateMutationInputArg!) { update_CertifiedAssignmentState(docID: $id, input: $input) { _docID } }",
        &json!({"id": id, "input": input}),
    ).await?;
    let rows = crate::graphql::mutation_documents(&response, "update_CertifiedAssignmentState")?
        .ok_or(ReceiverError::InvalidState("update omitted rows"))?;
    if rows.len() != 1 {
        return Err(ReceiverError::InvalidState(
            "update did not affect exactly one state",
        ));
    }
    Ok(())
}

async fn create_state(
    txn: &ConfigApplyTxn<'_>,
    key: &str,
    subject: &str,
    scope: &AssignmentScope,
    mut input: Value,
) -> Result<(), ReceiverError> {
    let object = input
        .as_object_mut()
        .ok_or(ReceiverError::InvalidState("invalid state input"))?;
    object.extend(
        json!({
            "scope_key": key, "owner_did": subject, "policy_id": scope.policy,
            "resource": scope.resource, "relation": scope.relation, "issuer": scope.issuer,
        })
        .as_object()
        .ok_or(ReceiverError::InvalidState("invalid scope input"))?
        .clone(),
    );
    let response = txn.execute_with_variables(
        "mutation($input: CertifiedAssignmentStateMutationInputArg!) { create_CertifiedAssignmentState(input: $input) { _docID } }",
        &json!({"input": input}),
    ).await?;
    let id = crate::graphql::created_doc_id(&response, "CertifiedAssignmentState")?;
    txn.add_resource_relationship(
        "CertifiedAssignmentState",
        &id,
        "parent",
        "nodes",
        &scope.node_document,
    )
    .await?;
    let actor = identity::Did::new(subject.to_owned())
        .map_err(|_| ReceiverError::InvalidState("invalid subject identity"))?;
    txn.add_actor_relationship("CertifiedAssignmentState", &id, "writer", &actor)
        .await?;
    Ok(())
}

fn stop_for_shutdown(requested: bool, closed: bool) -> bool {
    requested || closed
}

async fn interruptible<T>(
    future: impl Future<Output = T>,
    shutdown: &mut Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Option<T> {
    let Some(shutdown) = shutdown.as_deref_mut() else {
        return Some(future.await);
    };
    tokio::pin!(future);
    loop {
        if stop_for_shutdown(*shutdown.borrow(), false) {
            return None;
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if stop_for_shutdown(*shutdown.borrow(), changed.is_err()) { return None; }
            }
            result = &mut future => return Some(result),
        }
    }
}

async fn stop_invalid_work(
    runtime: &impl AssignmentRuntime,
    view: &AssignmentState,
    shutdown: &mut Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Result<bool, ReceiverError> {
    let Some(active) = interruptible(runtime.active(), shutdown).await else {
        return Ok(false);
    };
    let active = active.map_err(ReceiverError::Runtime)?;
    for id in &active {
        if !view.assigned.contains(id)
            || !match interruptible(runtime.has_verified_body(id), shutdown).await {
                Some(result) => result.map_err(ReceiverError::Runtime)?,
                None => return Ok(false),
            }
        {
            let Some(result) = interruptible(runtime.stop(id), shutdown).await else {
                return Ok(false);
            };
            result.map_err(ReceiverError::Runtime)?;
        }
    }
    Ok(true)
}

impl ConfigAccess {
    pub async fn watch_assignments<R>(
        &self,
        scope: &AssignmentScope,
        runtime: &R,
        collection_ids: Vec<String>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ReceiverError>
    where
        R: AssignmentRuntime,
    {
        let ConfigAccess::Local(local) = self else {
            return Err(ReceiverError::Provider(AssignmentReadError::Unsupported));
        };
        let changes = crate::subscription_source::subscribe(local.node(), collection_ids);
        let heights = local
            .node()
            .subscribe(&[events::EventName::AcpHeightAdvanced]);
        self.watch_assignment_events(scope, runtime, changes, heights, shutdown)
            .await
    }

    pub(crate) async fn watch_assignment_events(
        &self,
        scope: &AssignmentScope,
        runtime: &impl AssignmentRuntime,
        mut changes: crate::subscription_source::ChangeSubscription,
        mut heights: events::Subscription,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ReceiverError> {
        // The observer announces its first root with a height event only.
        let mut awaiting_first_height = true;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            while heights.try_recv().is_ok() {
                awaiting_first_height = false;
            }
            let retry = match self
                .refresh_from(scope, runtime, Some(&mut shutdown), |query| {
                    self.certified_assignments(query)
                })
                .await
            {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => false,
                Err(error) if error.retryable() => true,
                Err(error) => return Err(error),
            };
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                event = changes.recv() => {
                    if event.is_none() { return Err(ReceiverError::SubscriptionEnded); }
                }
                event = heights.recv(), if awaiting_first_height || retry => {
                    awaiting_first_height = false;
                    if event.is_none() { return Err(ReceiverError::SubscriptionEnded); }
                }
                _ = tokio::time::sleep(RETRY_INTERVAL), if retry => {}
            }
        }
    }

    pub async fn load_assignments(
        &self,
        scope: &AssignmentScope,
    ) -> Result<Option<AssignmentState>, ReceiverError> {
        let subject = self.actor_did().ok_or(ReceiverError::MissingIdentity)?;
        let key = scope_key(&subject, scope);
        self.transact_readonly("assignments.load", |txn| {
            let key = &key;
            Box::pin(async move {
                Ok::<_, ReceiverError>(load(txn, key).await?.map(|stored| stored.state))
            })
        })
        .await
    }

    pub async fn refresh_assignments<R>(
        &self,
        scope: &AssignmentScope,
        runtime: &R,
    ) -> Result<AssignmentState, ReceiverError>
    where
        R: AssignmentRuntime,
    {
        self.refresh_from(scope, runtime, None, |query| {
            self.certified_assignments(query)
        })
        .await?
        .ok_or(ReceiverError::InvalidState("unexpected shutdown"))
    }

    #[cfg(test)]
    pub(crate) async fn refresh_with_assignment_view(
        &self,
        scope: &AssignmentScope,
        runtime: &impl AssignmentRuntime,
        shutdown: Option<&mut tokio::sync::watch::Receiver<bool>>,
        read: impl Future<Output = Result<defra_node::CertifiedAssignments, AssignmentReadError>>,
    ) -> Result<Option<AssignmentState>, ReceiverError> {
        self.refresh_from(scope, runtime, shutdown, |_| read).await
    }

    async fn refresh_from<F>(
        &self,
        scope: &AssignmentScope,
        runtime: &impl AssignmentRuntime,
        mut shutdown: Option<&mut tokio::sync::watch::Receiver<bool>>,
        read: impl FnOnce(defra_node::AssignmentQuery) -> F,
    ) -> Result<Option<AssignmentState>, ReceiverError>
    where
        F: Future<Output = Result<defra_node::CertifiedAssignments, AssignmentReadError>>,
    {
        let subject = self.actor_did().ok_or(ReceiverError::MissingIdentity)?;
        let key = scope_key(&subject, scope);
        let ConfigAccess::Local(local) = self else {
            return Err(ReceiverError::Provider(AssignmentReadError::Unsupported));
        };
        let lock = local.assignment_lock(&key);
        let Some(_guard) = interruptible(lock.lock(), &mut shutdown).await else {
            return Ok(None);
        };
        // Earlier homes have only the applied row. It is their initial read floor.
        let floor_key = format!("floor:{key}");
        let (accepted, applied) = self
            .transact_readonly("assignments.floor", |txn| {
                let floor_key = &floor_key;
                let key = &key;
                Box::pin(async move {
                    let accepted = load(txn, floor_key).await?.map(|row| row.state);
                    let applied = load(txn, key).await?.map(|row| row.state);
                    Ok::<_, ReceiverError>((accepted, applied))
                })
            })
            .await?;
        if let Some(pending) = accepted
            .as_ref()
            .filter(|view| Some(*view) != applied.as_ref())
        {
            // An interrupted stop is still owed while the authority is unreachable.
            if !stop_invalid_work(runtime, pending, &mut shutdown).await? {
                return Ok(None);
            }
        }
        let previous = accepted.or(applied);
        let query = defra_node::AssignmentQuery {
            policy: scope.policy.clone(),
            resource: scope.resource.clone(),
            relation: scope.relation.clone(),
            subject: subject.clone(),
            issuer: scope.issuer.clone(),
            not_before: previous.as_ref().map(|state| state.position.clone()),
        };
        let Some(response) = interruptible(read(query), &mut shutdown).await else {
            return Ok(None);
        };
        let certified = response?;
        let mut assigned = certified.object_ids;
        assigned.sort();
        assigned.dedup();
        let next = AssignmentState {
            position: certified.position,
            assigned,
        };
        if let Some(previous) = &previous {
            check(previous, &next)?;
        }
        // Recovery must reject an older proof even if the process dies inside stop().
        self.transact_write("assignments.accept", |txn| {
            Box::pin(persist(txn, &floor_key, &subject, scope, &next))
        })
        .await?;
        if !stop_invalid_work(runtime, &next, &mut shutdown).await? {
            return Ok(None);
        }
        let committed = self
            .transact_write("assignments.persist", |txn| {
                Box::pin(persist(txn, &key, &subject, scope, &next))
            })
            .await?;
        let Some(active) = interruptible(runtime.active(), &mut shutdown).await else {
            return Ok(None);
        };
        let active = active.map_err(ReceiverError::Runtime)?;
        for id in &committed.assigned {
            if !active.contains(id) {
                let Some(result) =
                    interruptible(runtime.start_if_verified(id), &mut shutdown).await
                else {
                    return Ok(None);
                };
                result.map_err(ReceiverError::Runtime)?;
            }
        }
        Ok(Some(committed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_closed_shutdown_channel_prevents_a_ready_callback() {
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        drop(sender);
        let mut shutdown = Some(&mut receiver);
        let callback = async { panic!("callback ran after shutdown closed") };
        assert_eq!(interruptible::<()>(callback, &mut shutdown).await, None);
    }

    #[tokio::test]
    async fn false_shutdown_update_preserves_the_pending_callback() {
        use std::task::{Context, Poll};
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let pending = async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<()>().await
        };
        let mut shutdown = Some(&mut receiver);
        let future = interruptible(pending, &mut shutdown);
        tokio::pin!(future);
        let waker = futures::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        sender.send(false).unwrap();
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        sender.send(true).unwrap();
        assert!(matches!(
            future.as_mut().poll(&mut context),
            Poll::Ready(None)
        ));
    }

    fn state(term: u64, height: u64, hash: &str, assigned: &[&str]) -> AssignmentState {
        AssignmentState {
            position: defra_node::AssignmentPosition {
                term,
                height,
                block_hash: hash.into(),
            },
            assigned: assigned.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    #[test]
    fn an_old_view_cannot_restore_a_withdrawn_assignment() {
        let before = state(1, 10, "old", &["B"]);
        let after = state(1, 11, "new", &["C"]);
        assert!(matches!(
            check(&after, &before),
            Err(ReceiverError::Overtaken)
        ));
        assert!(!after.may_run("B", true));
        assert!(!after.may_run("C", false));
        assert!(after.may_run("C", true));
        assert!(matches!(
            check(&after, &state(1, 11, "wrong", &["B"])),
            Err(ReceiverError::InvalidState(_))
        ));
        assert!(check(&after, &state(2, 1, "term", &[])).is_ok());
    }

    #[test]
    fn a_parent_change_cannot_reset_the_revision_floor() {
        let mut scope = AssignmentScope {
            policy: "policy".into(),
            resource: "resource".into(),
            relation: "relation".into(),
            issuer: "issuer".into(),
            node_document: "first".into(),
        };
        let key = scope_key("subject", &scope);
        scope.node_document = "second".into();
        assert_eq!(scope_key("subject", &scope), key);
    }

    #[test]
    fn only_temporary_assignment_failures_are_retried() {
        let unavailable = ReceiverError::Provider(AssignmentReadError::Provider(
            defra_node::Error::DacPolicy(db::DacPolicyError::Assignments(
                acp::AssignmentReadError::Unavailable("offline".into()),
            )),
        ));
        let invalid = ReceiverError::Provider(AssignmentReadError::Provider(
            defra_node::Error::DacPolicy(db::DacPolicyError::Assignments(
                acp::AssignmentReadError::Invalid("invalid proof".into()),
            )),
        ));
        assert!(unavailable.retryable());
        assert!(ReceiverError::Overtaken.retryable());
        assert!(!invalid.retryable());
        assert!(!ReceiverError::Provider(AssignmentReadError::Unsupported).retryable());
    }

    #[test]
    fn assignment_admission_matches_the_model() {
        gents_lean_vocab::families::assignment_receiver_stop_for_shutdown::replay(|input| {
            stop_for_shutdown(input.requested, input.closed)
        });
        gents_lean_vocab::families::assignment_receiver_retry::replay(|input| {
            retry(input.unavailable, input.overtaken)
        });
        gents_lean_vocab::families::assignment_receiver_permits::replay(|input| {
            permits(input.assigned, input.body_verified)
        });
        gents_lean_vocab::families::assignment_receiver_may_run::replay(|input| {
            let state = AssignmentState {
                position: defra_node::AssignmentPosition {
                    term: 0,
                    height: 0,
                    block_hash: String::new(),
                },
                assigned: input.state.assigned.clone(),
            };
            state.may_run(&input.id, input.body_verified)
        });
        use gents_lean_vocab::families::assignment_receiver_observe as family;
        family::replay(|input| {
            if input.kind != "complete" {
                return input.state.clone();
            }
            let next = AssignmentState {
                position: defra_node::AssignmentPosition {
                    term: input.position.term,
                    height: input.position.height,
                    block_hash: input.position.hash.clone(),
                },
                assigned: input.assigned.clone(),
            };
            if let Some(previous) = &input.state.position {
                let previous = AssignmentState {
                    position: defra_node::AssignmentPosition {
                        term: previous.term,
                        height: previous.height,
                        block_hash: previous.hash.clone(),
                    },
                    assigned: input.state.assigned.clone(),
                };
                if check(&previous, &next).is_err() {
                    return input.state.clone();
                }
            }
            family::State {
                position: Some(input.position.clone()),
                assigned: input.assigned.clone(),
            }
        });
    }
}
