use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use super::{
    assignments::AssignmentReadError, ConfigAccess, ConfigApplyTxn, ConfigClientError,
    TransactionFailure,
};

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
    #[error(transparent)]
    Store(#[from] ConfigClientError),
    #[error(transparent)]
    Response(#[from] crate::graphql::GraphqlResponseError),
    #[error("assignment runtime failed")]
    Runtime(#[source] Box<dyn Error + Send + Sync>),
    #[error("assignment change subscription ended")]
    SubscriptionEnded,
}

#[async_trait::async_trait]
/// One assignment scope. Implementations verify the body and issuer signatures
/// before reporting it available, make start and stop safe to repeat, and leave
/// no delayed effects after a call returns or is cancelled. Starting must bind
/// the verified body and its issuer signatures to the work actually started.
pub trait AssignmentRuntime: Send + Sync {
    async fn active(&self) -> Result<Vec<String>, Box<dyn Error + Send + Sync>>;
    async fn stop(&self, id: &str) -> Result<(), Box<dyn Error + Send + Sync>>;
    async fn has_verified_body(&self, id: &str) -> Result<bool, Box<dyn Error + Send + Sync>>;
    async fn start_if_verified(&self, id: &str) -> Result<bool, Box<dyn Error + Send + Sync>>;
}

type ScopeLocks = Mutex<HashMap<(usize, String), Weak<tokio::sync::Mutex<()>>>>;
static SCOPE_LOCKS: OnceLock<ScopeLocks> = OnceLock::new();

fn scope_lock(
    access: &ConfigAccess,
    key: &str,
) -> Result<Arc<tokio::sync::Mutex<()>>, ReceiverError> {
    let ConfigAccess::Local(local) = access else {
        return Err(ReceiverError::Provider(AssignmentReadError::Unsupported));
    };
    let node = Arc::as_ptr(local.node()) as usize;
    let mut locks = SCOPE_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    let entry = locks.entry((node, key.to_owned())).or_default();
    if let Some(lock) = entry.upgrade() {
        return Ok(lock);
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    *entry = Arc::downgrade(&lock);
    Ok(lock)
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

impl ConfigAccess {
    pub async fn watch_assignments<R: AssignmentRuntime>(
        &self,
        scope: &AssignmentScope,
        runtime: &R,
        collection_ids: Vec<String>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ReceiverError> {
        let ConfigAccess::Local(local) = self else {
            return Err(ReceiverError::Provider(AssignmentReadError::Unsupported));
        };
        let mut changes = crate::subscription_source::subscribe(local.node(), collection_ids);
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            self.refresh_assignments(scope, runtime).await?;
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                event = changes.recv() => {
                    if event.is_none() { return Err(ReceiverError::SubscriptionEnded); }
                }
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

    pub async fn refresh_assignments<R: AssignmentRuntime>(
        &self,
        scope: &AssignmentScope,
        runtime: &R,
    ) -> Result<AssignmentState, ReceiverError> {
        let subject = self.actor_did().ok_or(ReceiverError::MissingIdentity)?;
        let key = scope_key(&subject, scope);
        let lock = scope_lock(self, &key)?;
        let _guard = lock.lock().await;
        let previous = self.load_assignments(scope).await?;
        let query = defra_node::AssignmentQuery {
            policy: scope.policy.clone(),
            resource: scope.resource.clone(),
            relation: scope.relation.clone(),
            subject: subject.clone(),
            issuer: scope.issuer.clone(),
            not_before: previous.map(|state| state.position),
        };
        let certified = self.certified_assignments(query).await?;
        let mut assigned = certified.object_ids;
        assigned.sort();
        assigned.dedup();
        let next = AssignmentState {
            position: certified.position,
            assigned,
        };
        let active = runtime.active().await.map_err(ReceiverError::Runtime)?;
        for id in &active {
            if !next.assigned.contains(id)
                || !runtime
                    .has_verified_body(id)
                    .await
                    .map_err(ReceiverError::Runtime)?
            {
                runtime.stop(id).await.map_err(ReceiverError::Runtime)?;
            }
        }
        let committed = self
            .transact_write("assignments.persist", |txn| {
                Box::pin(persist(txn, &key, &subject, scope, &next))
            })
            .await?;
        let active = runtime.active().await.map_err(ReceiverError::Runtime)?;
        for id in &committed.assigned {
            if !active.contains(id) {
                runtime
                    .start_if_verified(id)
                    .await
                    .map_err(ReceiverError::Runtime)?;
            }
        }
        Ok(committed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn assignment_admission_matches_the_model() {
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
