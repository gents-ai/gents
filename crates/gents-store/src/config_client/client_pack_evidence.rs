use std::sync::Arc;

use db::block::builder::{
    ClientCreateEvidence, ClientCreateSignatures, ClientDeleteSignatures, ClientMutationEvidence,
    ClientUpdateSignatures, ClientUpdateSnapshot, VerifiedClientCreate, VerifiedClientMutation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::client_signed_agent::{signing_schema, trusted_delete_schema};
use super::{ClientSigningCapability, ConfigAccess, ConfigApplyTxn, ConfigClientError};

/// An exact mutation independently selected by the pack installation plan.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackMutation {
    Create {
        collection: String,
        input: Value,
    },
    Update {
        collection: String,
        doc_id: String,
        input: Value,
    },
    Delete {
        collection: String,
        doc_id: String,
    },
}

/// One way two plans disagree, stated so an operator can see what to fix.
/// A refusal names these instead of only saying the graphs differ.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackMutationDifference {
    /// One plan creates, updates or deletes, the other does not.
    Operation {
        constructed: PackMutationKind,
        proposed: PackMutationKind,
    },
    /// Same operation on a different collection.
    Collection {
        constructed: String,
        proposed: String,
    },
    /// Same operation, different document: the shapes diverged before the
    /// input, which holds whenever the plan allocates an id.
    DocId {
        constructed: Option<String>,
        proposed: Option<String>,
    },
    /// The same field holds different values. `path` is a JSON pointer into the
    /// mutation input, so a reader can find the field in both plans. Both sides
    /// are [`ValueShape`]s, never values: this type is `Serialize` and `Debug`,
    /// so a `Value` here could reach a log or a model through either.
    Field {
        path: String,
        constructed: ValueShape,
        proposed: ValueShape,
    },
    /// A field only one of the plans carries.
    OnlyInOnePlan {
        path: String,
        constructed: Option<ValueShape>,
        proposed: Option<ValueShape>,
    },
}

/// A JSON value stated by kind and size, never by content.
///
/// Part of [`PackMutationDifference`] so the difference type holds no `Value` at
/// all: an install value can be a token from the installer's environment, and a
/// derived `Debug` or a `Serialize` of the difference would print it. A shape is
/// enough to see *where* and *what kind* two plans differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueShape {
    Null,
    Boolean,
    Number,
    /// A string of this many characters.
    Text {
        chars: usize,
    },
    /// An array of this many items.
    List {
        items: usize,
    },
    /// An object of this many fields.
    Map {
        fields: usize,
    },
}

impl ValueShape {
    /// The shape of `value`, recording only its kind and size.
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Boolean,
            Value::Number(_) => Self::Number,
            Value::String(text) => Self::Text {
                chars: text.chars().count(),
            },
            Value::Array(items) => Self::List { items: items.len() },
            Value::Object(fields) => Self::Map {
                fields: fields.len(),
            },
        }
    }

    /// How a line states it: `null`, `a string (32 chars)`, `an object (3
    /// fields)`.
    pub fn describe(self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Boolean => "a boolean".to_owned(),
            Self::Number => "a number".to_owned(),
            Self::Text { chars } => format!("a string ({chars} chars)"),
            Self::List { items } => format!("an array ({items} items)"),
            Self::Map { fields } => format!("an object ({fields} fields)"),
        }
    }
}

impl PackMutationDifference {
    /// One line a person can read, naming both sides.
    ///
    /// **No value is ever printed.** An install value can be a token from the
    /// installer's environment, and `${VAR}` is resolved into documents, so a
    /// secret can sit at any path — not only under `/variables`. This text
    /// reaches the operator, `runtime-errors.log` and models, so each side is
    /// stated by its kind and size: a JSON path (key names only, which is safe)
    /// and `string(32)`-style descriptors. A reader sees *where* and *what kind*
    /// the plans differ without seeing either value.
    pub fn describe(&self) -> String {
        match self {
            Self::Operation {
                constructed,
                proposed,
            } => format!(
                "the client's plan {} while the runtime's {}",
                constructed.verb(),
                proposed.verb()
            ),
            Self::Collection {
                constructed,
                proposed,
            } => format!(
                "the client's plan targets collection `{constructed}` while the runtime's targets `{proposed}`"
            ),
            Self::DocId {
                constructed,
                proposed,
            } => format!(
                "the plans name different documents (client {}, runtime {})",
                describe_doc_id(constructed.as_deref()),
                describe_doc_id(proposed.as_deref())
            ),
            Self::Field {
                path,
                constructed,
                proposed,
            } => format!(
                "`{path}` is {} in the client's plan and {} in the runtime's",
                constructed.describe(),
                proposed.describe()
            ),
            Self::OnlyInOnePlan {
                path,
                constructed,
                proposed,
            } => match (constructed, proposed) {
                (Some(value), None) => format!(
                    "only the client's plan carries `{path}` ({})",
                    value.describe()
                ),
                (None, Some(value)) => format!(
                    "only the runtime's plan carries `{path}` ({})",
                    value.describe()
                ),
                (Some(constructed), Some(proposed)) => format!(
                    "`{path}` is {} in the client's plan and {} in the runtime's",
                    constructed.describe(),
                    proposed.describe()
                ),
                (None, None) => format!("`{path}` is in neither plan"),
            },
        }
    }
}

/// How many differences a comparison reports. The first is the one that
/// matters; the rest are context, and a long list stops being readable.
pub const MAX_REPORTED_DIFFERENCES: usize = 5;

/// A document id is an identifier, not a value, so it may be named; a plan
/// that allocates one has none to name.
fn describe_doc_id(doc_id: Option<&str>) -> &str {
    doc_id.unwrap_or("a new one")
}

/// A JSON pointer token, escaped as RFC 6901 requires, so a path is unambiguous.
fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// What a plan comparison needs of a mutation, so the same diff reads a
/// document graph and a relationship graph.
pub trait PackMutationDiff {
    fn operation(&self) -> PackMutationKind;
    fn collection(&self) -> &str;
    fn doc_id(&self) -> Option<&str>;
    fn input(&self) -> Option<&Value>;

    /// Every way `self` (the client's plan) and `proposed` (the runtime's)
    /// disagree, in a stable order: operation, collection, document, then the
    /// input's fields in the order the client's plan states them. Bounded, so a
    /// large graph reports readably; the first entry is the first difference.
    fn differences(&self, proposed: &Self) -> Vec<PackMutationDifference> {
        let mut differences = Vec::new();
        if self.operation() != proposed.operation() {
            differences.push(PackMutationDifference::Operation {
                constructed: self.operation(),
                proposed: proposed.operation(),
            });
            return differences;
        }
        if self.collection() != proposed.collection() {
            differences.push(PackMutationDifference::Collection {
                constructed: self.collection().to_owned(),
                proposed: proposed.collection().to_owned(),
            });
        }
        if self.doc_id() != proposed.doc_id() {
            differences.push(PackMutationDifference::DocId {
                constructed: self.doc_id().map(str::to_owned),
                proposed: proposed.doc_id().map(str::to_owned),
            });
        }
        if let (Some(constructed), Some(proposed)) = (self.input(), proposed.input()) {
            collect_field_differences("", constructed, proposed, &mut differences);
        }
        differences.truncate(MAX_REPORTED_DIFFERENCES);
        differences
    }
}

/// The operation a mutation performs, without its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackMutationKind {
    Create,
    Update,
    Delete,
}

impl PackMutationKind {
    fn verb(self) -> &'static str {
        match self {
            Self::Create => "creates",
            Self::Update => "updates",
            Self::Delete => "deletes",
        }
    }
}

/// The first way two JSON documents differ, in one line, for a comparison whose
/// values are not a [`PackMutation`] (a relationship, say). Values are stated by
/// shape, never by content, as everywhere in this comparison.
pub fn describe_json_disagreement(constructed: &Value, proposed: &Value) -> String {
    let mut differences = Vec::new();
    collect_field_differences("", constructed, proposed, &mut differences);
    let total = differences.len();
    match differences.first() {
        Some(first) if total == 1 => first.describe(),
        Some(first) => format!(
            "{} (and {} more difference{})",
            first.describe(),
            total - 1,
            if total == 2 { "" } else { "s" }
        ),
        None => "the two agree field by field, so the difference is outside them".to_owned(),
    }
}

impl PackMutationDiff for PackMutation {
    fn operation(&self) -> PackMutationKind {
        match self {
            Self::Create { .. } => PackMutationKind::Create,
            Self::Update { .. } => PackMutationKind::Update,
            Self::Delete { .. } => PackMutationKind::Delete,
        }
    }

    fn collection(&self) -> &str {
        match self {
            Self::Create { collection, .. }
            | Self::Update { collection, .. }
            | Self::Delete { collection, .. } => collection,
        }
    }

    fn doc_id(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Update { doc_id, .. } | Self::Delete { doc_id, .. } => Some(doc_id),
        }
    }

    fn input(&self) -> Option<&Value> {
        match self {
            Self::Create { input, .. } | Self::Update { input, .. } => Some(input),
            Self::Delete { .. } => None,
        }
    }
}

/// Recurses into the two JSON values, recording the first leaf under each path
/// that differs -- as a [`ValueShape`], never the value, so no `Debug` or
/// `Serialize` of the difference can print a token. Objects are walked in the
/// client's key order, then any key only the runtime carries.
fn collect_field_differences(
    path: &str,
    constructed: &Value,
    proposed: &Value,
    differences: &mut Vec<PackMutationDifference>,
) {
    match (constructed, proposed) {
        (Value::Object(constructed), Value::Object(proposed)) => {
            for (key, value) in constructed {
                let child = format!("{path}/{}", escape_pointer_token(key));
                match proposed.get(key) {
                    Some(other) => collect_field_differences(&child, value, other, differences),
                    None => differences.push(PackMutationDifference::OnlyInOnePlan {
                        path: child,
                        constructed: Some(ValueShape::of(value)),
                        proposed: None,
                    }),
                }
            }
            for (key, value) in proposed {
                if !constructed.contains_key(key) {
                    differences.push(PackMutationDifference::OnlyInOnePlan {
                        path: format!("{path}/{}", escape_pointer_token(key)),
                        constructed: None,
                        proposed: Some(ValueShape::of(value)),
                    });
                }
            }
        }
        (Value::Array(constructed), Value::Array(proposed)) => {
            for (index, value) in constructed.iter().enumerate() {
                let child = format!("{path}/{index}");
                match proposed.get(index) {
                    Some(other) => collect_field_differences(&child, value, other, differences),
                    None => differences.push(PackMutationDifference::OnlyInOnePlan {
                        path: child,
                        constructed: Some(ValueShape::of(value)),
                        proposed: None,
                    }),
                }
            }
            for (index, value) in proposed.iter().enumerate().skip(constructed.len()) {
                differences.push(PackMutationDifference::OnlyInOnePlan {
                    path: format!("{path}/{index}"),
                    constructed: None,
                    proposed: Some(ValueShape::of(value)),
                });
            }
        }
        (constructed, proposed) if constructed != proposed => {
            differences.push(PackMutationDifference::Field {
                path: path.to_owned(),
                constructed: ValueShape::of(constructed),
                proposed: ValueShape::of(proposed),
            });
        }
        _ => {}
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "evidence", rename_all = "snake_case")]
enum PackGraphEvidence {
    Create(ClientCreateEvidence),
    Update(ClientMutationEvidence),
    Delete(ClientMutationEvidence),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackEvidence {
    scope: PackApprovalScope,
    actor_did: String,
    mutation: PackMutation,
    graph: PackGraphEvidence,
    signature: Vec<u8>,
}

/// Identifies the runtime-hosted stage that a client has reviewed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackApprovalScope {
    pub node_did: String,
    pub pack_digests: Vec<String>,
    pub nonce: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackRelationshipTarget {
    Actor {
        did: String,
    },
    Resource {
        resource: String,
        doc_id: String,
    },
    EntitySet {
        resource: String,
        doc_id: String,
        relation: String,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackRelationship {
    pub collection: String,
    pub doc_id: String,
    pub relation: String,
    pub target: PackRelationshipTarget,
    pub delete: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackRelationshipEvidence {
    scope: PackApprovalScope,
    actor_did: String,
    relationship: PackRelationship,
    signature: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackSchemaEvidence {
    scope: PackApprovalScope,
    actor_did: String,
    signature: Vec<u8>,
}

fn schema_payload(scope: &PackApprovalScope, actor: &str) -> Result<Vec<u8>, PackEvidenceError> {
    validate_scope(scope)?;
    Ok(serde_json::to_vec(&(
        "gents.pack.schemas.v1",
        scope,
        actor,
    ))?)
}

/// Approves schema assets from the exact reviewed archive graph for this Node.
pub async fn prepare_pack_schema_approval(
    actor: &ClientSigningCapability,
    scope: &PackApprovalScope,
) -> Result<PackSchemaEvidence, PackEvidenceError> {
    let signature = sign_client_payload(actor, schema_payload(scope, actor.did())?).await?;
    Ok(PackSchemaEvidence {
        scope: scope.clone(),
        actor_did: actor.did().to_owned(),
        signature,
    })
}

pub fn verify_pack_schema_approval(
    actor: &str,
    scope: &PackApprovalScope,
    evidence: PackSchemaEvidence,
) -> Result<(), PackEvidenceError> {
    if evidence.actor_did != actor || evidence.scope != *scope {
        return Err(PackEvidenceError::Mismatch);
    }
    let payload = schema_payload(scope, actor)?;
    if !crate::identity::verify_did_signature(actor, &payload, &evidence.signature)? {
        return Err(PackEvidenceError::Mismatch);
    }
    Ok(())
}

pub(super) enum VerifiedPackEvidence {
    Create(Arc<VerifiedClientCreate>),
    Mutation(Arc<VerifiedClientMutation>),
}

#[derive(Debug, thiserror::Error)]
pub enum PackEvidenceError {
    #[error("{0}")]
    Config(#[source] Box<ConfigClientError>),
    #[error("pack mutation does not use a trusted configuration schema")]
    UnsupportedCollection,
    #[error("pack mutation evidence does not match its operation or actor")]
    Mismatch,
    #[error("building or verifying client create evidence")]
    Create(#[from] db::block::builder::ClientCreateError),
    #[error("building client update evidence")]
    Update(#[from] db::block::builder::ClientUpdateError),
    #[error("building client deletion evidence")]
    Delete(#[from] db::block::builder::ClientDeleteError),
    #[error("verifying client mutation evidence")]
    Verify(#[from] db::block::builder::VerifiedMutationError),
    #[error("joining client evidence work")]
    Join(#[from] tokio::task::JoinError),
    #[error("encoding the approved pack relationship")]
    Encoding(#[from] serde_json::Error),
    #[error("signing or verifying the approved pack relationship")]
    Identity(#[source] Box<crate::identity::IdentityError>),
}

impl From<ConfigClientError> for PackEvidenceError {
    fn from(error: ConfigClientError) -> Self {
        Self::Config(Box::new(error))
    }
}

impl super::TransactionFailure for PackEvidenceError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

impl From<crate::identity::IdentityError> for PackEvidenceError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(Box::new(error))
    }
}

fn relationship_payload(
    scope: &PackApprovalScope,
    actor_did: &str,
    relationship: &PackRelationship,
) -> Result<Vec<u8>, PackEvidenceError> {
    validate_scope(scope)?;
    Ok(serde_json::to_vec(&(
        "gents.pack.relationship.v1",
        scope,
        actor_did,
        relationship,
    ))?)
}

fn validate_scope(scope: &PackApprovalScope) -> Result<(), PackEvidenceError> {
    if scope.node_did.is_empty()
        || scope.pack_digests.is_empty()
        || scope.pack_digests.iter().any(String::is_empty)
        || scope.nonce.is_empty()
    {
        return Err(PackEvidenceError::Mismatch);
    }
    Ok(())
}

/// Binds the exact signed graph using SHA-256 collision resistance.
fn graph_payload(
    scope: &PackApprovalScope,
    actor_did: &str,
    mutation: &PackMutation,
    graph: &PackGraphEvidence,
) -> Result<Vec<u8>, PackEvidenceError> {
    validate_scope(scope)?;
    let bytes = serde_json::to_vec(&("gents.pack.graph.v1", scope, actor_did, mutation, graph))?;
    Ok(Sha256::digest(bytes).to_vec())
}

/// Approve one plan-selected relationship with the client's key.
pub async fn prepare_pack_relationship(
    actor: &ClientSigningCapability,
    scope: &PackApprovalScope,
    relationship: &PackRelationship,
) -> Result<PackRelationshipEvidence, PackEvidenceError> {
    let payload = relationship_payload(scope, actor.did(), relationship)?;
    let signature = sign_client_payload(actor, payload).await?;
    Ok(PackRelationshipEvidence {
        scope: scope.clone(),
        actor_did: actor.did().to_owned(),
        relationship: relationship.clone(),
        signature,
    })
}

async fn sign_client_payload(
    actor: &ClientSigningCapability,
    payload: Vec<u8>,
) -> Result<Vec<u8>, PackEvidenceError> {
    Ok(actor.sign(payload).await?)
}

pub(super) fn verify_pack_relationship(
    actor: &str,
    scope: &PackApprovalScope,
    evidence: PackRelationshipEvidence,
) -> Result<PackRelationship, PackEvidenceError> {
    if evidence.actor_did != actor || evidence.scope != *scope {
        return Err(PackEvidenceError::Mismatch);
    }
    let payload = relationship_payload(scope, actor, &evidence.relationship)?;
    if !crate::identity::verify_did_signature(actor, &payload, &evidence.signature)? {
        return Err(PackEvidenceError::Mismatch);
    }
    Ok(evidence.relationship)
}

async fn snapshot(
    txn: &ConfigApplyTxn<'_>,
    doc_id: &str,
) -> Result<ClientUpdateSnapshot, ConfigClientError> {
    let request = defra_http::client_signed_update::history_request(doc_id);
    let value = txn
        .execute_with_variables(
            &request.query,
            &request.variables.unwrap_or_else(|| serde_json::json!({})),
        )
        .await?;
    let response: query::QueryResponse =
        serde_json::from_value(value).map_err(ConfigClientError::ClientSignedSnapshotDecode)?;
    defra_http::client_signed_update::snapshot_from_response(doc_id, &response)
        .map_err(ConfigClientError::ClientConfigurationGraph)
}

fn collection(mutation: &PackMutation) -> &str {
    match mutation {
        PackMutation::Create { collection, .. }
        | PackMutation::Update { collection, .. }
        | PackMutation::Delete { collection, .. } => collection,
    }
}

fn trusted_schema(mutation: &PackMutation) -> Result<schema::CollectionVersion, PackEvidenceError> {
    let name = collection(mutation);
    let sdl = trusted_delete_schema(name).ok_or(PackEvidenceError::UnsupportedCollection)?;
    Ok(signing_schema(sdl, name)?)
}

/// Sign the caller's already validated pack plan with the held client key.
pub async fn prepare_pack_evidence(
    access: &ConfigAccess,
    actor: &ClientSigningCapability,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
) -> Result<PackEvidence, PackEvidenceError> {
    if access.actor_did().as_deref() != Some(actor.did()) {
        return Err(PackEvidenceError::Mismatch);
    }
    let observed = match mutation {
        PackMutation::Create { .. } => None,
        PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => Some(
            access
                .transact_readonly("pack.prepare_client_evidence", |txn| {
                    Box::pin(async move { snapshot(txn, doc_id).await })
                })
                .await?,
        ),
    };
    prepare_pack_evidence_observed(actor, scope, mutation, observed).await
}

/// Use the caller's transaction snapshot, including its earlier staged writes.
pub async fn prepare_pack_evidence_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: &ClientSigningCapability,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
) -> Result<PackEvidence, PackEvidenceError> {
    if txn.actor_did() != Some(actor.did()) {
        return Err(PackEvidenceError::Mismatch);
    }
    let observed = match mutation {
        PackMutation::Create { .. } => None,
        PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => {
            Some(snapshot(txn, doc_id).await?)
        }
    };
    prepare_pack_evidence_observed(actor, scope, mutation, observed).await
}

/// Build from the transaction's history response, verifying its native blocks
/// before the held client key signs the exact operation.
pub async fn prepare_pack_evidence_from_history(
    actor: &ClientSigningCapability,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
    history: Option<Value>,
) -> Result<PackEvidence, PackEvidenceError> {
    let observed = match (mutation, history) {
        (PackMutation::Create { .. }, None) => None,
        (
            PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. },
            Some(history),
        ) => {
            let doc_id = doc_id.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    let response: query::QueryResponse = serde_json::from_value(history)
                        .map_err(ConfigClientError::ClientSignedSnapshotDecode)?;
                    defra_http::client_signed_update::snapshot_from_response(&doc_id, &response)
                        .map_err(ConfigClientError::ClientConfigurationGraph)
                })
                .await??,
            )
        }
        _ => return Err(PackEvidenceError::Mismatch),
    };
    prepare_pack_evidence_observed(actor, scope, mutation, observed).await
}

async fn prepare_pack_evidence_observed(
    actor: &ClientSigningCapability,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
    observed: Option<ClientUpdateSnapshot>,
) -> Result<PackEvidence, PackEvidenceError> {
    let schema = trusted_schema(mutation)?;
    let requested = mutation.clone();
    let signing = (*actor.config).clone();
    let graph =
        tokio::task::spawn_blocking(move || -> Result<PackGraphEvidence, PackEvidenceError> {
            let crdts = storage::Crdts::default();
            match requested {
                PackMutation::Create { input, .. } => Ok(PackGraphEvidence::Create(
                    ClientCreateSignatures::from_input(&crdts, &schema, &input, &signing)?
                        .evidence(),
                )),
                PackMutation::Update { doc_id, input, .. } => Ok(PackGraphEvidence::Update(
                    ClientUpdateSignatures::from_input(
                        &crdts,
                        &schema,
                        &doc_id,
                        &input,
                        observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                        &signing,
                    )?
                    .evidence(),
                )),
                PackMutation::Delete { doc_id, .. } => Ok(PackGraphEvidence::Delete(
                    ClientDeleteSignatures::from_snapshot(
                        &schema,
                        &doc_id,
                        observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                        &signing,
                    )?
                    .evidence(),
                )),
            }
        })
        .await??;
    let payload_scope = scope.clone();
    let payload_actor = actor.did().to_owned();
    let payload_mutation = mutation.clone();
    let (graph, payload) = tokio::task::spawn_blocking(move || {
        let payload = graph_payload(&payload_scope, &payload_actor, &payload_mutation, &graph)?;
        Ok::<_, PackEvidenceError>((graph, payload))
    })
    .await??;
    let signature = sign_client_payload(actor, payload).await?;
    Ok(PackEvidence {
        scope: scope.clone(),
        actor_did: actor.did().to_owned(),
        mutation: mutation.clone(),
        graph,
        signature,
    })
}

/// Verify the plan's exact mutation against the transaction's native snapshot.
pub(super) async fn verify_pack_evidence(
    txn: &ConfigApplyTxn<'_>,
    actor: &str,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
    evidence: PackEvidence,
) -> Result<VerifiedPackEvidence, PackEvidenceError> {
    if txn.actor_did() != Some(actor)
        || evidence.actor_did != actor
        || evidence.scope != *scope
        || evidence.mutation != *mutation
    {
        return Err(PackEvidenceError::Mismatch);
    }
    let checked_scope = scope.clone();
    let checked_actor = actor.to_owned();
    let checked_mutation = mutation.clone();
    let evidence = tokio::task::spawn_blocking(move || {
        let payload = graph_payload(
            &checked_scope,
            &checked_actor,
            &checked_mutation,
            &evidence.graph,
        )?;
        if !crate::identity::verify_did_signature(&checked_actor, &payload, &evidence.signature)? {
            return Err(PackEvidenceError::Mismatch);
        }
        Ok::<_, PackEvidenceError>(evidence)
    })
    .await??;
    let schema = trusted_schema(mutation)?;
    let observed = match mutation {
        PackMutation::Create { .. } => None,
        PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => {
            Some(snapshot(txn, doc_id).await?)
        }
    };
    let mutation = mutation.clone();
    let actor = actor.to_owned();
    tokio::task::spawn_blocking(move || {
        let crdts = storage::Crdts::default();
        match (mutation, evidence.graph) {
            (PackMutation::Create { input, .. }, PackGraphEvidence::Create(evidence)) => {
                let graph = VerifiedClientCreate::verify(&crdts, &schema, &input, evidence)?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Create(Arc::new(graph)))
            }
            (PackMutation::Update { doc_id, input, .. }, PackGraphEvidence::Update(evidence)) => {
                let graph = VerifiedClientMutation::verify_update(
                    &crdts,
                    &schema,
                    &doc_id,
                    &input,
                    observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                    evidence,
                )?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Mutation(Arc::new(graph)))
            }
            (PackMutation::Delete { doc_id, .. }, PackGraphEvidence::Delete(evidence)) => {
                let graph = VerifiedClientMutation::verify_delete(
                    &schema,
                    &doc_id,
                    observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                    evidence,
                )?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Mutation(Arc::new(graph)))
            }
            _ => Err(PackEvidenceError::Mismatch),
        }
    })
    .await?
}

/// Apply only the graph verified for this exact plan operation and actor.
pub(super) async fn apply_pack_evidence(
    txn: &ConfigApplyTxn<'_>,
    mutation: &PackMutation,
    graph: VerifiedPackEvidence,
) -> Result<Value, PackEvidenceError> {
    let _ = trusted_schema(mutation)?;
    let result = match (mutation, graph) {
        (PackMutation::Create { collection, input }, VerifiedPackEvidence::Create(graph)) => {
            let document = format!("mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}");
            txn.execute_verified_client_create(
                &document,
                &serde_json::json!({"input": input}),
                graph,
            )
            .await?
        }
        (
            PackMutation::Update {
                collection,
                doc_id,
                input,
            },
            VerifiedPackEvidence::Mutation(graph),
        ) => {
            let doc_id = crate::graphql::escape_graphql_string(doc_id);
            let document = format!("mutation($input: {collection}MutationInputArg!) {{ update_{collection}(docID: \"{doc_id}\", input: $input) {{ _docID }} }}");
            txn.execute_verified_client_mutation(
                &document,
                &serde_json::json!({"input": input}),
                graph,
            )
            .await?
        }
        (PackMutation::Delete { collection, doc_id }, VerifiedPackEvidence::Mutation(graph)) => {
            txn.delete_verified_configuration_document(collection, doc_id, graph)
                .await?;
            serde_json::json!({"data": {(format!("delete_{collection}")): [{"_docID": doc_id}]}})
        }
        _ => return Err(PackEvidenceError::Mismatch),
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{KeyIdentity, NodeIdentity};
    use serde_json::json;

    #[tokio::test]
    async fn hosted_schema_approval_conforms_to_lean() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let user = KeyIdentity::load_or_create(dir.path().join("user"), None)?;
        let other = KeyIdentity::load_or_create(dir.path().join("other"), None)?;
        let runtime = KeyIdentity::load_or_create(dir.path().join("runtime"), None)?;
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(runtime.did().to_owned())
                .build()
                .await?,
        );
        crate::schema::ensure_runtime_schemas(&node).await?;
        let user_access = ConfigAccess::local_as(node.clone(), user.did().parse()?);
        let node_access = ConfigAccess::local_as(node, runtime.did().parse()?);
        let user_did = identity::Did::new(user.did())?;
        for permission in [
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DocumentDelete,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            node_access
                .grant_node_permission(&user_did, permission)
                .await?;
        }
        let other_did = identity::Did::new(other.did())?;
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DacRelationAdd,
        ] {
            node_access
                .grant_node_permission(&other_did, permission)
                .await?;
        }
        let scope = PackApprovalScope {
            node_did: runtime.did().into(),
            pack_digests: vec!["sha256:fixture".into()],
            nonce: "session".into(),
        };
        let create = PackMutation::Create {
            collection: "Node".into(),
            input: json!({"node_did": runtime.did(), "enabled": true}),
        };
        let user_key = user.client_signing_capability()?;
        let create_evidence =
            prepare_pack_evidence(&user_access, &user_key, &scope, &create).await?;
        user_access
            .transact_verified_pack("test.hosted_schema.node", |txn| {
                let create_evidence = create_evidence.clone();
                let create = &create;
                let scope = &scope;
                let runtime = &runtime;
                let user_key = &user_key;
                Box::pin(async move {
                    let result = txn.apply(scope, create, create_evidence).await?;
                    let id = crate::graphql::created_doc_id(&result, "Node")?;
                    let relationship = PackRelationship {
                        collection: "Node".into(),
                        doc_id: id,
                        relation: "runtime_reader".into(),
                        target: PackRelationshipTarget::Actor {
                            did: runtime.did().into(),
                        },
                        delete: false,
                    };
                    let signed = prepare_pack_relationship(user_key, scope, &relationship).await?;
                    txn.apply_relationship(scope, &relationship, signed).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let signed = prepare_pack_schema_approval(&user_key, &scope).await?;
        for case in gents_lean_vocab::families::contracts_pack_installation_hosted_schema::cases() {
            let scope_matches = case.input.scope_matches;
            let signature_valid = case.input.signed;
            let native_update = case.input.native_update;
            let subject = if native_update { &user } else { &other };
            let mut evidence = signed.clone();
            if !signature_valid {
                evidence.signature.clear();
            }
            if !scope_matches {
                evidence.scope.nonce.push_str("-other");
            }
            // The actor binding remains valid when exercising native denial.
            if !native_update {
                evidence =
                    prepare_pack_schema_approval(&other.client_signing_capability()?, &scope)
                        .await?;
                if !signature_valid {
                    evidence.signature.clear();
                }
                if !scope_matches {
                    evidence.scope.nonce.push_str("-other");
                }
            }
            let valid = verify_pack_schema_approval(subject.did(), &scope, evidence).is_ok();
            let observed = node_access
                .transact_readonly("test.hosted_schema.authorize", |txn| {
                    let runtime = &runtime;
                    Box::pin(async move {
                        super::super::require_node_update_in_txn(
                            txn,
                            &identity::Did::new(runtime.did())?,
                            &identity::Did::new(subject.did())?,
                        )
                        .await
                        .map_err(anyhow::Error::from)
                    })
                })
                .await
                .is_ok();
            assert_eq!(observed, native_update);
            case.check(valid && observed);
        }
        Ok(())
    }

    #[tokio::test]
    async fn verified_pack_evidence_binds_mutation_actor_and_snapshot() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let actor = KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
        let runtime = KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
        let other = KeyIdentity::load_or_create(directory.path().join("other.key"), None)?;
        let actor_key = actor.client_signing_capability()?;
        let other_key = other.client_signing_capability()?;
        let mut public_only = defra_core::signing::get_identity(actor.did()).unwrap();
        public_only.private_key_bytes.clear();
        defra_core::signing::store_identity(actor.did(), public_only);
        assert!(!defra_core::signing::get_identity(actor.did())
            .unwrap()
            .has_local_private_key());
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(actor.did().to_owned())
                .build()
                .await?,
        );
        crate::schema::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(node.clone(), actor.did().parse()?);
        let scope = PackApprovalScope {
            node_did: runtime.did().to_owned(),
            pack_digests: vec!["sha256:fixture".into()],
            nonce: "test-session".into(),
        };
        let node_create = PackMutation::Create {
            collection: "Node".into(),
            input: json!({"node_did": runtime.did(), "enabled": true}),
        };
        let node_evidence =
            prepare_pack_evidence(&access, &actor_key, &scope, &node_create).await?;
        let node_doc = access
            .transact_verified_pack("test.pack_evidence.node", |txn| {
                let node_create = &node_create;
                let evidence = node_evidence.clone();
                let scope = &scope;
                Box::pin(async move {
                    let response = txn.apply(scope, node_create, evidence).await?;
                    Ok::<_, anyhow::Error>(crate::graphql::created_doc_id(&response, "Node")?)
                })
            })
            .await?;
        let create = PackMutation::Create {
            collection: "PackInstallation".into(),
            input: json!({"node_did": runtime.did(), "coordinate": "acme/fixture", "version": "1", "digest": "one"}),
        };
        let evidence = prepare_pack_evidence(&access, &actor_key, &scope, &create).await?;
        let other_access = ConfigAccess::local_as(node.clone(), other.did().parse()?);
        let other_evidence =
            prepare_pack_evidence(&other_access, &other_key, &scope, &create).await?;
        let cases =
            gents_lean_vocab::families::contracts_pack_installation_hosted_evidence::cases();
        let tampered = PackMutation::Create {
            collection: "PackInstallation".into(),
            input: json!({"node_did": runtime.did(), "coordinate": "acme/fixture", "version": "1", "digest": "other"}),
        };
        let (created, stale_expected) = access
            .transact_verified_pack("test.pack_evidence.create", |txn| {
                let evidence = evidence.clone();
                let other_evidence = other_evidence.clone();
                let create = &create;
                let tampered = &tampered;
                let cases = &cases;
                let actor = &actor_key;
                let other = &other_key;
                let node_doc = &node_doc;
                let scope = &scope;
                Box::pin(async move {
                    let mut response = None;
                    let mut admitted = Vec::new();
                    for case in cases {
                        let actor_matches = case.input.actor_matches;
                        let graph_verified = case.input.graph_verified;
                        let plan_approved = case.input.plan_approved;
                        let candidate = if graph_verified { create } else { tampered };
                        let signed = if actor_matches {
                            evidence.clone()
                        } else {
                            other_evidence.clone()
                        };
                        let mut wrong_scope = (*scope).clone();
                        wrong_scope.nonce.push_str("-other");
                        let actual = txn
                            .apply(
                                if plan_approved { scope } else { &wrong_scope },
                                candidate,
                                signed,
                            )
                            .await;
                        case.check(actual.is_ok());
                        admitted.push((
                            (actor_matches, graph_verified, plan_approved),
                            actual.is_ok(),
                        ));
                        if let Ok(value) = actual {
                            response = Some(value);
                        }
                    }
                    let admission = |actor: bool, graph: bool, plan: bool| {
                        admitted
                            .iter()
                            .find(|(key, _)| *key == (actor, graph, plan))
                            .expect("modeled case")
                            .1
                    };
                    let response = response.expect("modeled successful create");
                    let created = crate::graphql::created_doc_id(&response, "PackInstallation")?;
                    let parent = PackRelationship {
                        collection: "PackInstallation".into(),
                        doc_id: created.clone(),
                        relation: "parent".into(),
                        target: PackRelationshipTarget::Resource {
                            resource: "nodes".into(),
                            doc_id: node_doc.to_owned(),
                        },
                        delete: false,
                    };
                    let signed = prepare_pack_relationship(actor, scope, &parent).await?;
                    let mut altered = signed.clone();
                    altered.relationship.relation = "reader".into();
                    let expected = admission(true, false, true);
                    assert_eq!(
                        txn.apply_relationship(scope, &parent, altered)
                            .await
                            .is_ok(),
                        expected
                    );
                    let other_signed = prepare_pack_relationship(other, scope, &parent).await?;
                    let expected = admission(false, true, true);
                    assert_eq!(
                        txn.apply_relationship(scope, &parent, other_signed)
                            .await
                            .is_ok(),
                        expected
                    );
                    let mut wrong_scope = (*scope).clone();
                    wrong_scope.nonce.push_str("-other");
                    let expected = admission(true, true, false);
                    assert_eq!(
                        txn.apply_relationship(&wrong_scope, &parent, signed.clone())
                            .await
                            .is_ok(),
                        expected
                    );
                    txn.apply_relationship(scope, &parent, signed).await?;
                    Ok::<_, anyhow::Error>((created, admission(true, false, true)))
                })
            })
            .await?;
        let update = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: created,
            input: json!({"version": "2"}),
        };
        let evidence = access
            .transact_readonly("test.pack_evidence.prepare_in_txn", |txn| {
                let actor = &actor_key;
                let scope = &scope;
                let update = &update;
                Box::pin(
                    async move { prepare_pack_evidence_in_txn(txn, actor, scope, update).await },
                )
            })
            .await?;
        access
            .transact_verified_pack("test.pack_evidence.update", |txn| {
                let evidence = evidence.clone();
                let update = &update;
                let scope = &scope;
                Box::pin(async move {
                    txn.apply(scope, update, evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let observed = access
            .execute("{ PackInstallation { coordinate version } }")
            .await?;
        assert_eq!(observed["data"]["PackInstallation"][0]["version"], "2");
        let stale = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: match &update {
                PackMutation::Update { doc_id, .. } => doc_id.clone(),
                _ => unreachable!(),
            },
            input: json!({"version": "stale"}),
        };
        let stale_evidence = prepare_pack_evidence(&access, &actor_key, &scope, &stale).await?;
        let replacement = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: match &update {
                PackMutation::Update { doc_id, .. } => doc_id.clone(),
                _ => unreachable!(),
            },
            input: json!({"version": "winner"}),
        };
        let replacement_evidence =
            prepare_pack_evidence(&access, &actor_key, &scope, &replacement).await?;
        access
            .transact_verified_pack("test.pack_evidence.winner", |txn| {
                let replacement_evidence = replacement_evidence.clone();
                let replacement = &replacement;
                let scope = &scope;
                Box::pin(async move {
                    txn.apply(scope, replacement, replacement_evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let stale_result = access
            .transact_verified_pack("test.pack_evidence.stale", |txn| {
                let evidence = stale_evidence.clone();
                let stale = &stale;
                let scope = &scope;
                Box::pin(async move {
                    txn.apply(scope, stale, evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        assert_eq!(stale_result.is_ok(), stale_expected);
        let observed = access
            .execute("{ PackInstallation { coordinate version } }")
            .await?;
        assert_eq!(observed["data"]["PackInstallation"][0]["version"], "winner");
        node.shutdown().await?;
        Ok(())
    }

    fn create(collection: &str, input: serde_json::Value) -> PackMutation {
        PackMutation::Create {
            collection: collection.to_owned(),
            input,
        }
    }

    /// The refusal has to name what to fix, not only that two plans differ.
    #[test]
    fn a_differing_field_is_named_with_both_values() {
        let constructed = create(
            "Agent",
            serde_json::json!({"agent_did": "did:key:client", "display_name": "Scout"}),
        );
        let proposed = create(
            "Agent",
            serde_json::json!({"agent_did": "did:key:runtime", "display_name": "Scout"}),
        );
        let differences = constructed.differences(&proposed);
        assert_eq!(
            differences,
            vec![PackMutationDifference::Field {
                path: "/agent_did".to_owned(),
                constructed: ValueShape::Text { chars: 14 },
                proposed: ValueShape::Text { chars: 15 },
            }],
            "the one differing leaf is named, and the agreeing field is not"
        );
        let described = differences[0].describe();
        assert!(described.contains("/agent_did"), "{described}");
        // The value never prints -- only its kind and size.
        assert!(!described.contains("did:key:client"), "{described}");
        assert!(!described.contains("did:key:runtime"), "{described}");
        assert!(described.contains("chars)"), "{described}");
    }

    /// A field only one plan carries is a difference a value comparison alone
    /// would miss.
    #[test]
    fn a_field_only_one_plan_carries_is_named() {
        let constructed = create("Agent", serde_json::json!({"a": 1}));
        let proposed = create("Agent", serde_json::json!({"a": 1, "b": 2}));
        assert_eq!(
            constructed.differences(&proposed),
            vec![PackMutationDifference::OnlyInOnePlan {
                path: "/b".to_owned(),
                constructed: None,
                proposed: Some(ValueShape::Number),
            }]
        );
        assert!(constructed.differences(&proposed)[0]
            .describe()
            .contains("runtime"));
    }

    /// An operation or collection difference is stated before any field, since
    /// nothing in the input can agree once the shapes differ.
    #[test]
    fn an_operation_or_collection_difference_comes_first() {
        let created = create("Agent", serde_json::json!({"a": 1}));
        let deleted = PackMutation::Delete {
            collection: "Agent".to_owned(),
            doc_id: "bafy1".to_owned(),
        };
        assert_eq!(
            created.differences(&deleted),
            vec![PackMutationDifference::Operation {
                constructed: PackMutationKind::Create,
                proposed: PackMutationKind::Delete,
            }]
        );
        let other_collection = create("Context", serde_json::json!({"a": 1}));
        let differences = created.differences(&other_collection);
        assert_eq!(
            differences.first(),
            Some(&PackMutationDifference::Collection {
                constructed: "Agent".to_owned(),
                proposed: "Context".to_owned(),
            })
        );
    }

    /// Arrays are walked by index, so a difference inside one is named by its
    /// path rather than reported as "the array differs".
    #[test]
    fn a_difference_inside_an_array_is_named_by_its_index() {
        let constructed = create("Agent", serde_json::json!({"tags": ["a", "b"]}));
        let proposed = create("Agent", serde_json::json!({"tags": ["a", "c"]}));
        assert_eq!(
            constructed.differences(&proposed),
            vec![PackMutationDifference::Field {
                path: "/tags/1".to_owned(),
                constructed: ValueShape::Text { chars: 1 },
                proposed: ValueShape::Text { chars: 1 },
            }]
        );
    }

    /// Agreeing plans produce no difference, so the caller can tell "identical"
    /// from "differs in a way I could not name".
    #[test]
    fn agreeing_plans_have_no_differences() {
        let input = serde_json::json!({"a": 1, "b": ["x", {"c": true}]});
        assert!(create("Agent", input.clone())
            .differences(&create("Agent", input))
            .is_empty());
    }

    /// A JSON pointer escapes `~` and `/` so a path cannot be misread.
    #[test]
    fn a_pointer_token_escapes_the_pointer_characters() {
        let constructed = create("Agent", serde_json::json!({"a/b": 1, "c~d": 2}));
        let proposed = create("Agent", serde_json::json!({"a/b": 9, "c~d": 8}));
        let paths = constructed
            .differences(&proposed)
            .into_iter()
            .map(|difference| match difference {
                PackMutationDifference::Field { path, .. } => path,
                other => panic!("expected a field difference: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["/a~1b".to_owned(), "/c~0d".to_owned()]);
    }

    /// The relationship comparison goes through the same walk.
    #[test]
    fn two_json_documents_name_their_first_difference() {
        let described = describe_json_disagreement(
            &serde_json::json!({"relation": "reader", "subject": "did:key:a"}),
            &serde_json::json!({"relation": "reader", "subject": "did:key:b"}),
        );
        assert!(described.contains("/subject"), "{described}");
        // No value prints, here either: only where and what kind.
        assert!(!described.contains("did:key:a"), "{described}");
        assert!(!described.contains("did:key:b"), "{described}");
        assert!(
            describe_json_disagreement(&serde_json::json!({"a": 1}), &serde_json::json!({"a": 1}))
                .contains("agree"),
            "identical values say so"
        );
    }

    /// The shape that refused every hosted `gents/web` install: one plan states
    /// a variable as `null`, the other omits the key. Pinned as a fixture so
    /// the diff keeps naming it, whatever fixes the plans upstream.
    #[test]
    fn an_absent_key_against_a_null_one_is_named() {
        let constructed = create("PackInstallation", serde_json::json!({"variables": {}}));
        let proposed = create(
            "PackInstallation",
            serde_json::json!({"variables": {"GENTS_PACK_BIND_AGENT_WEB_RESEARCHER": null}}),
        );
        let differences = constructed.differences(&proposed);
        assert_eq!(
            differences,
            vec![PackMutationDifference::OnlyInOnePlan {
                path: "/variables/GENTS_PACK_BIND_AGENT_WEB_RESEARCHER".to_owned(),
                constructed: None,
                proposed: Some(ValueShape::Null),
            }],
            "absent against null is named at its own path, not as a whole-object difference"
        );
        let described = differences[0].describe();
        assert!(
            described.contains("GENTS_PACK_BIND_AGENT_WEB_RESEARCHER"),
            "{described}"
        );
        assert!(described.contains("runtime"), "{described}");
    }

    /// No value ever prints: an install value can be a token from the
    /// installer's environment, and this text reaches the operator, the log and
    /// models. The kind and size are enough to act on.
    #[test]
    fn an_install_value_prints_as_kind_and_size_never_as_text() {
        let secret = "sk-live-0123456789abcdef";
        let constructed = create(
            "PackInstallation",
            serde_json::json!({"variables": {"API_TOKEN": secret}}),
        );
        let proposed = create(
            "PackInstallation",
            serde_json::json!({"variables": {"API_TOKEN": "other"}}),
        );
        let described = constructed.differences(&proposed)[0].describe();
        assert!(
            !described.contains(secret),
            "the value must not print: {described}"
        );
        assert!(!described.contains("other"), "{described}");
        assert!(described.contains("/variables/API_TOKEN"), "{described}");
        assert!(
            described.contains("chars)"),
            "the kind and size are stated instead: {described}"
        );
    }

    /// A difference at exactly `/variables` -- null against a map, or the whole
    /// map missing -- must not print the map, which is where every value sits.
    #[test]
    fn a_difference_at_the_variables_object_prints_no_value() {
        let secret = "sk-live-0123456789abcdef";
        let with_map = create(
            "PackInstallation",
            serde_json::json!({"variables": {"API_TOKEN": secret}}),
        );
        let with_null = create("PackInstallation", serde_json::json!({"variables": null}));
        let described = with_map.differences(&with_null)[0].describe();
        assert!(!described.contains(secret), "{described}");
        assert!(described.contains("/variables"), "{described}");
        assert!(
            described.contains("null") && described.contains("fields"),
            "null against an object, by kind: {described}"
        );

        // And the whole key absent in one plan.
        let without = create("PackInstallation", serde_json::json!({"a": 1}));
        let has = create(
            "PackInstallation",
            serde_json::json!({"a": 1, "variables": {"API_TOKEN": secret}}),
        );
        let described = without.differences(&has)[0].describe();
        assert!(!described.contains(secret), "{described}");
        assert!(described.contains("/variables"), "{described}");
    }

    /// `${VAR}` is resolved into documents, so a token can sit at any path --
    /// an MCP header or an env value, say -- not only under `/variables`.
    #[test]
    fn a_rendered_field_holding_a_token_prints_no_value() {
        let token = "ghp_0123456789abcdefghij";
        let constructed = create(
            "McpService",
            serde_json::json!({"headers": {"Authorization": format!("Bearer {token}")}}),
        );
        let proposed = create(
            "McpService",
            serde_json::json!({"headers": {"Authorization": "Bearer other"}}),
        );
        let described = constructed.differences(&proposed)[0].describe();
        assert!(
            !described.contains(token),
            "the rendered token must not print: {described}"
        );
        assert!(!described.contains("Bearer"), "{described}");
        assert!(
            described.contains("/headers/Authorization"),
            "the path names where: {described}"
        );
        assert!(described.contains("chars)"), "{described}");
    }

    /// The list is bounded, so a plan that diverges in hundreds of leaves
    /// reports the first few rather than all of them.
    #[test]
    fn the_reported_differences_are_bounded() {
        let mut constructed_input = serde_json::Map::new();
        let mut proposed_input = serde_json::Map::new();
        for index in 0..40 {
            constructed_input.insert(format!("field_{index}"), serde_json::json!(index));
            proposed_input.insert(format!("field_{index}"), serde_json::json!("other"));
        }
        let differences = create("Agent", serde_json::Value::Object(constructed_input))
            .differences(&create("Agent", serde_json::Value::Object(proposed_input)));
        assert_eq!(
            differences.len(),
            super::MAX_REPORTED_DIFFERENCES,
            "at most {} differences are reported",
            super::MAX_REPORTED_DIFFERENCES
        );
    }

    /// B13b: the difference type holds **no** `Value` at all, so neither a
    /// derived `Debug` nor a `Serialize` can print a token -- not only the
    /// `describe` line, which a caller might not use.
    #[test]
    fn the_difference_type_prints_no_value_through_debug_or_serialize() {
        let secret = "sk-live-0123456789abcdef";
        let constructed = create(
            "PackInstallation",
            serde_json::json!({"variables": {"API_TOKEN": secret}}),
        );
        let proposed = create(
            "PackInstallation",
            serde_json::json!({"variables": {"API_TOKEN": "other"}}),
        );
        let differences = constructed.differences(&proposed);
        // Debug is what an `unwrap` or a `{:?}` prints.
        let debugged = format!("{differences:?}");
        assert!(!debugged.contains(secret), "Debug leaks: {debugged}");
        assert!(!debugged.contains("other"), "Debug leaks: {debugged}");
        // Serialize is what a log field or a JSON error body would carry.
        let serialized = serde_json::to_string(&differences).expect("differences serialize");
        assert!(
            !serialized.contains(secret),
            "Serialize leaks: {serialized}"
        );
        assert!(
            !serialized.contains("other"),
            "Serialize leaks: {serialized}"
        );
        // And the shape is still there to act on.
        assert!(serialized.contains("text"), "{serialized}");
        assert!(serialized.contains("chars"), "{serialized}");
    }

    /// `ValueShape` states kind and size for every JSON kind, so a difference
    /// never has to fall back to printing a value.
    #[test]
    fn every_json_kind_has_a_shape() {
        for (value, expected) in [
            (serde_json::json!(null), "null"),
            (serde_json::json!(true), "a boolean"),
            (serde_json::json!(7), "a number"),
            (serde_json::json!("abc"), "a string (3 chars)"),
            (serde_json::json!([1, 2]), "an array (2 items)"),
            (serde_json::json!({"a": 1}), "an object (1 fields)"),
        ] {
            assert_eq!(
                super::ValueShape::of(&value).describe(),
                expected,
                "shape of {value}"
            );
        }
    }
}
