use std::collections::HashSet;

use anyhow::{anyhow, Context as _, Result};
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest};
use identity::Did;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
struct CommitParentRow {
    cid: String,
    #[serde(rename = "fieldName")]
    field_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompositeHeadEvidenceRow {
    cid: String,
    #[serde(default)]
    heads: Vec<CommitParentRow>,
    #[serde(default)]
    signature: Option<DeclaredSignatureMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeclaredSignatureMetadata {
    #[serde(default)]
    identity: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompositeCommitLinkRow {
    cid: String,
    #[serde(rename = "docID")]
    doc_id: String,
    #[serde(rename = "fieldName")]
    field_name: Option<String>,
    #[serde(rename = "collectionVersionId")]
    collection_version_id: Option<String>,
    #[serde(default)]
    links: Vec<CommitParentRow>,
}

fn sole_current_composite_head<'a>(
    rows: &'a [CompositeHeadEvidenceRow],
    collection: &str,
    doc_id: &str,
) -> Result<&'a CompositeHeadEvidenceRow> {
    let nested_composite_cids = rows
        .iter()
        .flat_map(|row| row.heads.iter())
        .filter(|head| head.field_name.as_deref() == Some("_C"))
        .map(|head| head.cid.as_str())
        .collect::<HashSet<_>>();
    let current = rows
        .iter()
        .filter(|row| !nested_composite_cids.contains(row.cid.as_str()))
        .collect::<Vec<_>>();
    match current.as_slice() {
        [current] => Ok(*current),
        [] => anyhow::bail!("{collection} {doc_id} has no current composite head"),
        current => anyhow::bail!(
            "{collection} {doc_id} has {} current composite heads; refusing ambiguous provenance",
            current.len()
        ),
    }
}

/// Commit metadata reported by a GraphQL endpoint without local block
/// verification.
///
/// `declared_signature_identity` is deliberately not named `signer_did`:
/// remote `_commits.signature.identity` is commit metadata, not proof that the
/// block signature is valid. Only [`SignedDocumentVersionRef`] represents a
/// cryptographically verified author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnverifiedDocumentVersionMetadata {
    pub version: DocumentVersionRef,
    pub declared_signature_identity: Option<String>,
}

/// Parse a complete `_commits` composite graph and select its sole current
/// composite version.
///
/// This parser is reusable by remote GraphQL readers, but its result remains
/// explicitly unverified. The graph must not have zero or multiple current
/// heads.
pub(crate) fn unverified_current_document_version_metadata_from_commits(
    commits: &Value,
    collection: &str,
    doc_id: &str,
) -> Result<UnverifiedDocumentVersionMetadata> {
    let rows = serde_json::from_value::<Vec<CompositeHeadEvidenceRow>>(commits.clone())?;
    let current = sole_current_composite_head(&rows, collection, doc_id)?;
    Ok(UnverifiedDocumentVersionMetadata {
        version: DocumentVersionRef::new(doc_id, &current.cid),
        declared_signature_identity: current
            .signature
            .as_ref()
            .and_then(|signature| signature.identity.clone()),
    })
}

pub(crate) fn current_composite_metadata_query(doc_id: &str) -> String {
    let escaped_doc_id = crate::graphql::escape_graphql_string(doc_id);
    format!(
        r#"query {{
            _commits(
                docID: ["{escaped_doc_id}"],
                filter: {{ fieldName: {{ _eq: "_C" }} }}
            ) {{
                cid
                heads {{ cid fieldName }}
                signature {{ identity }}
            }}
        }}"#
    )
}

/// Resolve the sole current composite version of one DefraDB document and
/// cryptographically verify the signer of that exact commit.
///
/// This is intentionally collection-agnostic. Callers remain responsible for
/// reloading the CID and validating collection-specific facts such as lifecycle
/// state and logical ownership before treating the version as admitted input.
pub(crate) async fn verified_current_signed_document_version(
    node: &EmbeddedNode,
    collection: &str,
    doc_id: &str,
) -> Result<SignedDocumentVersionRef> {
    verified_current_signed_document_version_with_identity(node, collection, doc_id, None).await
}

/// Transaction-scoped variant of
/// [`verified_current_signed_document_version`].
///
/// Correctness-sensitive writers use this before a mutation in the same
/// transaction. DefraDB's optimistic transaction validation then makes the
/// exact version check and the write one atomic decision: a concurrent head
/// change conflicts instead of allowing evidence for an older live version to
/// authorize a newer one.
pub(crate) async fn verified_current_signed_document_version_in_txn(
    node: &EmbeddedNode,
    transaction: &defra_node::TransactionHandle,
    collection: &str,
    doc_id: &str,
) -> Result<SignedDocumentVersionRef> {
    let query = current_composite_metadata_query(doc_id);
    let response = node
        .execute_request_in_txn(QueryRequest::new(query), transaction)
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying {collection} {doc_id} composite evidence in transaction failed: {:?}",
            response.errors
        );
    }
    let commits = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let current =
        unverified_current_document_version_metadata_from_commits(&commits, collection, doc_id)?;
    let signer_did = node
        .verified_block_signer_did_in_txn(&current.version.composite_commit_cid, transaction)
        .await
        .map_err(|error| {
            anyhow!(
                "cryptographically verifying {collection} {doc_id} composite commit {} in transaction: {error}",
                current.version.composite_commit_cid
            )
        })?;
    if signer_did.trim().is_empty() {
        anyhow::bail!(
            "cryptographically verifying {collection} {doc_id} composite commit {} in transaction returned an empty signer DID",
            current.version.composite_commit_cid
        );
    }
    Ok(SignedDocumentVersionRef::new(current.version, signer_did))
}

/// Identity-aware variant used by correctness paths that will later be ACP
/// protected. The query identity is authorization context only; signer
/// verification below remains the authorship proof.
pub(crate) async fn verified_current_signed_document_version_with_identity(
    node: &EmbeddedNode,
    collection: &str,
    doc_id: &str,
    identity: Option<Did>,
) -> Result<SignedDocumentVersionRef> {
    let query = current_composite_metadata_query(doc_id);
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(query).with_identity(identity),
            ExecuteRetryPolicy::default(),
        )
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying {collection} {doc_id} composite evidence failed: {:?}",
            response.errors
        );
    }
    let commits = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let current =
        unverified_current_document_version_metadata_from_commits(&commits, collection, doc_id)?;
    let signer_did = node
        .verified_block_signer_did(&current.version.composite_commit_cid)
        .await
        .map_err(|error| {
            anyhow!(
                "cryptographically verifying {collection} {doc_id} composite commit {}: {error}",
                current.version.composite_commit_cid
            )
        })?;
    if signer_did.trim().is_empty() {
        anyhow::bail!(
            "cryptographically verifying {collection} {doc_id} composite commit {} returned an empty signer DID",
            current.version.composite_commit_cid
        );
    }
    Ok(SignedDocumentVersionRef::new(current.version, signer_did))
}

/// Backend-neutral current-version verifier used by correctness-sensitive
/// readers that must work through both an embedded node and authenticated
/// GraphQL. The executor is responsible for attaching query identity and for
/// cryptographically verifying the returned commit signer.
pub(crate) async fn verified_current_signed_document_version_with_executor(
    executor: &(impl crate::GraphqlExecutor + ?Sized),
    collection: &str,
    doc_id: &str,
) -> Result<SignedDocumentVersionRef> {
    let query = current_composite_metadata_query(doc_id);
    let response = executor.execute_graphql(&query).await?;
    if response.has_errors() {
        anyhow::bail!(
            "querying {collection} {doc_id} composite evidence failed: {:?}",
            response.errors
        );
    }
    let commits = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let current =
        unverified_current_document_version_metadata_from_commits(&commits, collection, doc_id)?;
    let signer_did = executor
        .verified_signer_did(&current.version.composite_commit_cid)
        .await
        .with_context(|| {
            format!(
                "cryptographically verifying {collection} {doc_id} composite commit {}",
                current.version.composite_commit_cid
            )
        })?;
    if signer_did.trim().is_empty() {
        anyhow::bail!(
            "cryptographically verifying {collection} {doc_id} composite commit {} returned an empty signer DID",
            current.version.composite_commit_cid
        );
    }
    Ok(SignedDocumentVersionRef::new(current.version, signer_did))
}

/// An immutable point in one DefraDB document's history.
///
/// `_docID` is the stable document identity. `composite_commit_cid` is the
/// content-addressed composite commit that reconstructs the exact snapshot the
/// runtime consumed. Neither value substitutes for the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentVersionRef {
    pub doc_id: String,
    pub composite_commit_cid: String,
}

impl DocumentVersionRef {
    pub(crate) fn new(doc_id: impl Into<String>, composite_commit_cid: impl Into<String>) -> Self {
        Self {
            doc_id: doc_id.into(),
            composite_commit_cid: composite_commit_cid.into(),
        }
    }
}

/// One exact DefraDB document version together with its cryptographically
/// verified commit author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedDocumentVersionRef {
    pub version: DocumentVersionRef,
    pub signer_did: String,
}

impl SignedDocumentVersionRef {
    pub(crate) fn new(version: DocumentVersionRef, signer_did: impl Into<String>) -> Self {
        Self {
            version,
            signer_did: signer_did.into(),
        }
    }
}

/// One document loaded at an exact composite commit, paired with the locally
/// and cryptographically verified author of that commit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VerifiedExactDocumentSnapshot {
    pub source: SignedDocumentVersionRef,
    /// The DefraDB collection schema that interpreted `document` at this
    /// exact composite commit. This is commit evidence, not the node's current
    /// active schema version.
    pub collection_version_id: String,
    pub document: Value,
}

impl VerifiedExactDocumentSnapshot {
    /// Deserialize the exact document into a collection-specific row type.
    pub(crate) fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.document.clone()).map_err(Into::into)
    }
}

pub(crate) fn unverified_exact_document_snapshot_from_data(
    data: &Value,
    collection: &str,
    version: &DocumentVersionRef,
) -> Result<Value> {
    let documents = data
        .get(collection)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!(
                "exact {collection} snapshot {} at {} did not return a document array",
                version.doc_id,
                version.composite_commit_cid
            )
        })?;
    let document = match documents.as_slice() {
        [document] => document,
        [] => anyhow::bail!(
            "exact {collection} snapshot {} at {} returned no document",
            version.doc_id,
            version.composite_commit_cid
        ),
        documents => anyhow::bail!(
            "exact {collection} snapshot {} at {} returned {} documents; refusing ambiguous history",
            version.doc_id,
            version.composite_commit_cid,
            documents.len()
        ),
    };
    let returned_doc_id = document
        .get("_docID")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "exact {collection} snapshot {} at {} omitted _docID",
                version.doc_id,
                version.composite_commit_cid
            )
        })?;
    if returned_doc_id != version.doc_id {
        anyhow::bail!(
            "exact {collection} snapshot at {} returned document {}, expected {}",
            version.composite_commit_cid,
            returned_doc_id,
            version.doc_id
        );
    }
    Ok(document.clone())
}

pub(crate) fn exact_document_snapshot_query(
    collection: &str,
    version: &DocumentVersionRef,
    selection_set: &str,
) -> Result<String> {
    crate::graphql::validate_collection_identifier(collection)?;
    if version.doc_id.trim().is_empty() || version.composite_commit_cid.trim().is_empty() {
        anyhow::bail!(
            "exact {collection} snapshot requires a document ID and composite commit CID"
        );
    }
    if selection_set.trim().is_empty() {
        anyhow::bail!("exact {collection} snapshot requires a non-empty field selection");
    }

    let escaped_doc_id = crate::graphql::escape_graphql_string(&version.doc_id);
    let escaped_cid = crate::graphql::escape_graphql_string(&version.composite_commit_cid);
    Ok(format!(
        r#"query {{
            _commits(
                cid: ["{escaped_cid}"],
                docID: ["{escaped_doc_id}"]
            ) {{
                cid
                docID
                fieldName
                collectionVersionId
            }}
            {collection}(
                cid: ["{escaped_cid}"],
                docID: ["{escaped_doc_id}"]
            ) {{
                _docID
                {selection_set}
            }}
        }}"#
    ))
}

/// Identity-aware exact-snapshot variant for ACP-protected reads. Query
/// identity is authorization context; the returned signer is still obtained
/// from local cryptographic block verification.
pub(crate) async fn verified_exact_document_snapshot_with_identity(
    node: &EmbeddedNode,
    collection: &str,
    version: &DocumentVersionRef,
    selection_set: &str,
    identity: Option<Did>,
) -> Result<VerifiedExactDocumentSnapshot> {
    let query = exact_document_snapshot_query(collection, version, selection_set)?;
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(query).with_identity(identity),
            ExecuteRetryPolicy::default(),
        )
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying exact {collection} snapshot {} at {} failed: {:?}",
            version.doc_id,
            version.composite_commit_cid,
            response.errors
        );
    }
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| anyhow!("querying exact {collection} snapshot returned no data"))?;
    let commits = data
        .get("_commits")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let collection_version_id =
        validate_composite_document_version_from_commits(&commits, collection, version)?;
    let document = unverified_exact_document_snapshot_from_data(data, collection, version)?;
    let signer_did = node
        .verified_block_signer_did(&version.composite_commit_cid)
        .await
        .map_err(|error| {
            anyhow!(
                "cryptographically verifying exact {collection} snapshot {} at {}: {error}",
                version.doc_id,
                version.composite_commit_cid
            )
        })?;
    if signer_did.trim().is_empty() {
        anyhow::bail!(
            "cryptographically verifying exact {collection} snapshot {} at {} returned an empty signer DID",
            version.doc_id,
            version.composite_commit_cid
        );
    }
    Ok(VerifiedExactDocumentSnapshot {
        source: SignedDocumentVersionRef::new(version.clone(), signer_did),
        collection_version_id,
        document,
    })
}

/// Backend-neutral exact-snapshot verifier. Unlike plain remote GraphQL
/// metadata, this only returns after the executor has cryptographically
/// verified the exact composite commit's signer.
pub(crate) async fn verified_exact_document_snapshot_with_executor(
    executor: &(impl crate::GraphqlExecutor + ?Sized),
    collection: &str,
    version: &DocumentVersionRef,
    selection_set: &str,
) -> Result<VerifiedExactDocumentSnapshot> {
    let query = exact_document_snapshot_query(collection, version, selection_set)?;
    let response = executor.execute_graphql(&query).await?;
    if response.has_errors() {
        anyhow::bail!(
            "querying exact {collection} snapshot {} at {} failed: {:?}",
            version.doc_id,
            version.composite_commit_cid,
            response.errors
        );
    }
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| anyhow!("querying exact {collection} snapshot returned no data"))?;
    let commits = data
        .get("_commits")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let collection_version_id =
        validate_composite_document_version_from_commits(&commits, collection, version)?;
    let document = unverified_exact_document_snapshot_from_data(data, collection, version)?;
    let signer_did = executor
        .verified_signer_did(&version.composite_commit_cid)
        .await
        .with_context(|| {
            format!(
                "cryptographically verifying exact {collection} snapshot {} at {}",
                version.doc_id, version.composite_commit_cid
            )
        })?;
    if signer_did.trim().is_empty() {
        anyhow::bail!(
            "cryptographically verifying exact {collection} snapshot {} at {} returned an empty signer DID",
            version.doc_id,
            version.composite_commit_cid
        );
    }
    Ok(VerifiedExactDocumentSnapshot {
        source: SignedDocumentVersionRef::new(version.clone(), signer_did),
        collection_version_id,
        document,
    })
}

/// Resolve and verify the DefraDB collection schema that interpreted one
/// already-signed exact document version.
///
/// Durable provenance must carry this value alongside the document CID. A
/// commit can only be decoded relative to its collection schema, so callers
/// must never infer it from the node's currently active schema.
pub(crate) async fn verified_collection_version_id_with_identity(
    node: &EmbeddedNode,
    collection: &str,
    source: &SignedDocumentVersionRef,
    identity: Option<Did>,
) -> Result<String> {
    crate::graphql::validate_collection_identifier(collection)?;
    if source.version.doc_id.trim().is_empty()
        || source.version.composite_commit_cid.trim().is_empty()
        || source.signer_did.trim().is_empty()
    {
        anyhow::bail!("{collection} exact source reference is incomplete");
    }
    let escaped_doc_id = crate::graphql::escape_graphql_string(&source.version.doc_id);
    let escaped_cid = crate::graphql::escape_graphql_string(&source.version.composite_commit_cid);
    let query = format!(
        r#"query {{
            _commits(
                cid: ["{escaped_cid}"],
                docID: ["{escaped_doc_id}"]
            ) {{
                cid
                docID
                fieldName
                collectionVersionId
            }}
        }}"#
    );
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(query).with_identity(identity),
            ExecuteRetryPolicy::default(),
        )
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying {collection} {} schema identity at {} failed: {:?}",
            source.version.doc_id,
            source.version.composite_commit_cid,
            response.errors
        );
    }
    let commits = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let collection_version_id =
        validate_composite_document_version_from_commits(&commits, collection, &source.version)?;
    let signer_did = node
        .verified_block_signer_did(&source.version.composite_commit_cid)
        .await
        .with_context(|| {
            format!(
                "cryptographically re-verifying {collection} {} schema identity at {}",
                source.version.doc_id, source.version.composite_commit_cid
            )
        })?;
    if signer_did != source.signer_did {
        anyhow::bail!(
            "{collection} {} schema identity signer {signer_did} disagrees with pinned signer {}",
            source.version.doc_id,
            source.signer_did
        );
    }
    Ok(collection_version_id)
}

/// The exact field commit selected by one composite document version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentFieldVersionRef {
    pub document_version: DocumentVersionRef,
    pub field_name: String,
    pub field_commit_cid: String,
}

fn validate_composite_document_version_from_commits(
    commits: &Value,
    collection: &str,
    version: &DocumentVersionRef,
) -> Result<String> {
    let rows = serde_json::from_value::<Vec<CompositeCommitLinkRow>>(commits.clone())?;
    let composite = match rows.as_slice() {
        [composite] => composite,
        [] => anyhow::bail!(
            "{collection} {} composite commit {} was not found",
            version.doc_id,
            version.composite_commit_cid
        ),
        rows => anyhow::bail!(
            "{collection} {} composite commit {} returned {} rows; refusing ambiguous provenance",
            version.doc_id,
            version.composite_commit_cid,
            rows.len()
        ),
    };
    if composite.cid != version.composite_commit_cid
        || composite.doc_id != version.doc_id
        || composite.field_name.as_deref() != Some("_C")
    {
        anyhow::bail!(
            "{collection} {} commit {} is not the requested composite document version",
            version.doc_id,
            version.composite_commit_cid
        );
    }
    let collection_version_id = composite
        .collection_version_id
        .as_deref()
        .filter(|version_id| !version_id.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "{collection} {} composite commit {} omitted collectionVersionId",
                version.doc_id,
                version.composite_commit_cid
            )
        })?;
    Ok(collection_version_id.to_string())
}

pub(crate) fn document_field_version_ref_from_commits(
    commits: &Value,
    collection: &str,
    version: &DocumentVersionRef,
    field_name: &str,
) -> Result<DocumentFieldVersionRef> {
    validate_composite_document_version_from_commits(commits, collection, version)?;
    let rows = serde_json::from_value::<Vec<CompositeCommitLinkRow>>(commits.clone())?;
    let composite = &rows[0];
    let matching = composite
        .links
        .iter()
        .filter(|link| link.field_name.as_deref() == Some(field_name))
        .collect::<Vec<_>>();
    let field_commit = match matching.as_slice() {
        [field_commit] => *field_commit,
        [] => anyhow::bail!(
            "{collection} {} composite commit {} has no {field_name} field link",
            version.doc_id,
            version.composite_commit_cid
        ),
        matching => anyhow::bail!(
            "{collection} {} composite commit {} has {} {field_name} field links; refusing ambiguous provenance",
            version.doc_id,
            version.composite_commit_cid,
            matching.len()
        ),
    };
    if field_commit.cid.trim().is_empty() {
        anyhow::bail!(
            "{collection} {} composite commit {} has an empty {field_name} field commit CID",
            version.doc_id,
            version.composite_commit_cid
        );
    }
    Ok(DocumentFieldVersionRef {
        document_version: version.clone(),
        field_name: field_name.to_string(),
        field_commit_cid: field_commit.cid.clone(),
    })
}

pub(crate) fn document_field_version_query(
    collection: &str,
    version: &DocumentVersionRef,
    field_name: &str,
) -> Result<String> {
    crate::graphql::validate_collection_identifier(collection)?;
    crate::graphql::validate_graphql_name(field_name)?;
    if version.doc_id.trim().is_empty() || version.composite_commit_cid.trim().is_empty() {
        anyhow::bail!("{collection} field version requires a document ID and composite commit CID");
    }

    let escaped_doc_id = crate::graphql::escape_graphql_string(&version.doc_id);
    let escaped_cid = crate::graphql::escape_graphql_string(&version.composite_commit_cid);
    Ok(format!(
        r#"query {{
            _commits(
                cid: ["{escaped_cid}"],
                docID: ["{escaped_doc_id}"]
            ) {{
                cid
                docID
                fieldName
                collectionVersionId
                links {{ cid fieldName }}
            }}
        }}"#
    ))
}

/// Identity-aware field-link lookup for ACP-protected commit metadata.
pub(crate) async fn document_field_version_ref_with_identity(
    node: &EmbeddedNode,
    collection: &str,
    version: &DocumentVersionRef,
    field_name: &str,
    identity: Option<Did>,
) -> Result<DocumentFieldVersionRef> {
    let query = document_field_version_query(collection, version, field_name)?;
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(query).with_identity(identity),
            ExecuteRetryPolicy::default(),
        )
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying {collection} {} composite commit {} for {field_name}: {:?}",
            version.doc_id,
            version.composite_commit_cid,
            response.errors
        );
    }
    let commits = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    document_field_version_ref_from_commits(&commits, collection, version, field_name)
}

/// One exact, signed configuration fact used during behavior resolution.
///
/// `collection` and `logical_id` retain the semantic edge that a bare DefraDB
/// document version cannot express. `source` is the cryptographically verified
/// immutable document snapshot from which the runtime parsed the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFactRef {
    pub collection: String,
    pub logical_id: String,
    pub collection_version_id: String,
    pub source: SignedDocumentVersionRef,
}

impl ConfigFactRef {
    pub(crate) fn new(
        collection: impl Into<String>,
        logical_id: impl Into<String>,
        collection_version_id: impl Into<String>,
        source: SignedDocumentVersionRef,
    ) -> Self {
        Self {
            collection: collection.into(),
            logical_id: logical_id.into(),
            collection_version_id: collection_version_id.into(),
            source,
        }
    }

    fn validate(&self, expected_collection: &str) -> anyhow::Result<()> {
        if self.collection != expected_collection {
            anyhow::bail!(
                "configuration fact collection {} does not match expected {expected_collection}",
                self.collection
            );
        }
        if self.logical_id.trim().is_empty() {
            anyhow::bail!("{expected_collection} configuration fact has an empty logical id");
        }
        if self.collection_version_id.trim().is_empty() {
            anyhow::bail!(
                "{expected_collection} {} configuration fact requires a collection version id",
                self.logical_id
            );
        }
        if self.source.version.doc_id.trim().is_empty()
            || self.source.version.composite_commit_cid.trim().is_empty()
        {
            anyhow::bail!(
                "{expected_collection} {} configuration fact requires a document id and composite commit CID",
                self.logical_id
            );
        }
        if self.source.signer_did.trim().is_empty() {
            anyhow::bail!(
                "{expected_collection} {} configuration fact requires a verified signer DID",
                self.logical_id
            );
        }
        Ok(())
    }
}

/// The exact signed document set used to resolve one behavior configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedBehaviorConfigProvenance {
    pub principal: ConfigFactRef,
    pub behavior: ConfigFactRef,
    pub inference_backend: ConfigFactRef,
    pub inference_profile: ConfigFactRef,
    pub tool_selection: Option<ConfigFactRef>,
    /// Linked datastore write surfaces in canonical ascending `logical_id`
    /// order. These definitions contribute directly to the provider tool
    /// surface and therefore must be pinned independently of ToolSelection.
    #[serde(default)]
    pub datastore_tool_surfaces: Vec<ConfigFactRef>,
    /// Effective skills in canonical ascending `logical_id` order.
    pub skills: Vec<ConfigFactRef>,
    pub resolution_algorithm_version: u32,
}

impl ResolvedBehaviorConfigProvenance {
    pub fn validate_for_behavior(&self, behavior_id: &str, agent_did: &str) -> anyhow::Result<()> {
        if behavior_id.trim().is_empty() {
            anyhow::bail!("behavior configuration provenance requires a behavior id");
        }
        if agent_did.trim().is_empty() {
            anyhow::bail!("behavior configuration provenance requires an agent DID");
        }
        if self.resolution_algorithm_version == 0 {
            anyhow::bail!("behavior configuration provenance requires a non-zero resolution algorithm version");
        }

        self.principal.validate("AgentPrincipal")?;
        self.behavior.validate("AgentBehavior")?;
        self.inference_backend.validate("InferenceBackend")?;
        self.inference_profile.validate("InferenceProfile")?;
        if let Some(tool_selection) = &self.tool_selection {
            tool_selection.validate("ToolSelection")?;
        }
        for surface in &self.datastore_tool_surfaces {
            surface.validate("DatastoreToolSurface")?;
        }
        for skill in &self.skills {
            skill.validate("Skill")?;
        }

        if self.principal.logical_id != agent_did {
            anyhow::bail!(
                "principal provenance {} does not match agent {agent_did}",
                self.principal.logical_id
            );
        }
        if self.behavior.logical_id != behavior_id {
            anyhow::bail!(
                "behavior provenance {} does not match behavior {behavior_id}",
                self.behavior.logical_id
            );
        }
        if let Some((left, right)) = self.datastore_tool_surfaces.windows(2).find_map(|pair| {
            (pair[0].logical_id >= pair[1].logical_id).then_some((&pair[0], &pair[1]))
        }) {
            anyhow::bail!(
                "datastore tool surface provenance must be unique and canonically ordered; found {} before {}",
                left.logical_id,
                right.logical_id
            );
        }
        if let Some((left, right)) = self.skills.windows(2).find_map(|pair| {
            (pair[0].logical_id >= pair[1].logical_id).then_some((&pair[0], &pair[1]))
        }) {
            anyhow::bail!(
                "skill provenance must be unique and canonically ordered; found {} before {}",
                left.logical_id,
                right.logical_id
            );
        }
        Ok(())
    }
}

/// Provenance boundary for one request execution.
///
/// `source` is the sole current composite head admitted before the claim.
/// `claim` is the exact target-agent-authored child version whose only
/// composite parent is `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestExecutionProvenance {
    pub source: SignedDocumentVersionRef,
    pub claim: SignedDocumentVersionRef,
}

impl RequestExecutionProvenance {
    pub(crate) fn new(source: SignedDocumentVersionRef, claim: SignedDocumentVersionRef) -> Self {
        Self { source, claim }
    }

    pub(crate) fn validate_for_request(
        &self,
        request_doc_id: &str,
        target_agent_did: &str,
    ) -> anyhow::Result<()> {
        if request_doc_id.trim().is_empty() {
            anyhow::bail!("request provenance requires a non-empty document id");
        }
        if target_agent_did.trim().is_empty() {
            anyhow::bail!("request provenance requires a non-empty target agent DID");
        }
        if self.source.version.doc_id != request_doc_id
            || self.claim.version.doc_id != request_doc_id
        {
            anyhow::bail!(
                "request provenance source and claim must both reference document {request_doc_id}"
            );
        }
        if self.source.version.composite_commit_cid.trim().is_empty()
            || self.claim.version.composite_commit_cid.trim().is_empty()
        {
            anyhow::bail!("request provenance source and claim CIDs must be non-empty");
        }
        if self.source.version.composite_commit_cid == self.claim.version.composite_commit_cid {
            anyhow::bail!("request provenance source and claim CIDs must be distinct");
        }
        if self.source.signer_did.trim().is_empty() || self.claim.signer_did.trim().is_empty() {
            anyhow::bail!("request provenance source and claim signer DIDs must be non-empty");
        }
        if self.claim.signer_did != target_agent_did {
            anyhow::bail!(
                "request provenance claim signer {} does not match target agent {}",
                self.claim.signer_did,
                target_agent_did
            );
        }
        Ok(())
    }
}

#[doc(hidden)]
pub(crate) fn test_request_execution_provenance(
    doc_id: &str,
    claim_signer_did: &str,
) -> RequestExecutionProvenance {
    RequestExecutionProvenance::new(
        SignedDocumentVersionRef::new(
            DocumentVersionRef::new(doc_id, "bafy-source-1"),
            "did:key:source",
        ),
        SignedDocumentVersionRef::new(
            DocumentVersionRef::new(doc_id, "bafy-claim-1"),
            claim_signer_did,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(cid: &str, parents: &[&str]) -> CompositeHeadEvidenceRow {
        CompositeHeadEvidenceRow {
            cid: cid.to_string(),
            heads: parents
                .iter()
                .map(|cid| CommitParentRow {
                    cid: (*cid).to_string(),
                    field_name: Some("_C".to_string()),
                })
                .collect(),
            signature: None,
        }
    }

    #[test]
    fn exact_provenance_requires_one_current_composite_head() {
        let empty = sole_current_composite_head(&[], "Skill", "doc-skill")
            .unwrap_err()
            .to_string();
        assert!(empty.contains("no current composite head"));

        let ambiguous_rows = [head("bafy-left", &[]), head("bafy-right", &[])];
        let ambiguous = sole_current_composite_head(&ambiguous_rows, "Skill", "doc-skill")
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("2 current composite heads"));

        let linear_rows = [
            head("bafy-parent", &[]),
            head("bafy-child", &["bafy-parent"]),
        ];
        assert_eq!(
            sole_current_composite_head(&linear_rows, "Skill", "doc-skill")
                .unwrap()
                .cid,
            "bafy-child"
        );
    }

    #[test]
    fn remote_commit_identity_remains_unverified_metadata() {
        let commits = serde_json::json!([
            {
                "cid": "bafy-parent",
                "heads": [],
                "signature": { "identity": "declared-key-parent" }
            },
            {
                "cid": "bafy-child",
                "heads": [{ "cid": "bafy-parent", "fieldName": "_C" }],
                "signature": { "identity": "declared-key-child" }
            }
        ]);
        let metadata = unverified_current_document_version_metadata_from_commits(
            &commits,
            "RenderedRequest",
            "doc-request",
        )
        .unwrap();

        assert_eq!(metadata.version.doc_id, "doc-request");
        assert_eq!(metadata.version.composite_commit_cid, "bafy-child");
        assert_eq!(
            metadata.declared_signature_identity.as_deref(),
            Some("declared-key-child")
        );
        assert!(!current_composite_metadata_query("doc-request").contains("limit"));
    }

    #[test]
    fn exact_snapshot_requires_one_matching_document() {
        let version = DocumentVersionRef::new("doc-request", "bafy-composite");
        let data = serde_json::json!({
            "RenderedRequest": [{
                "_docID": "doc-request",
                "request_json": "{\"model\":\"test\"}"
            }]
        });
        let snapshot =
            unverified_exact_document_snapshot_from_data(&data, "RenderedRequest", &version)
                .unwrap();
        assert_eq!(snapshot["_docID"], "doc-request");
        let query =
            exact_document_snapshot_query("RenderedRequest", &version, "request_json created_at")
                .unwrap();
        assert!(query.contains("cid: [\"bafy-composite\"]"));
        assert!(query.contains("docID: [\"doc-request\"]"));
        assert!(!query.contains("limit"));
        assert!(query.contains("fieldName"));
        assert!(query.contains("collectionVersionId"));

        let wrong_document = serde_json::json!({
            "RenderedRequest": [{ "_docID": "doc-other" }]
        });
        assert!(unverified_exact_document_snapshot_from_data(
            &wrong_document,
            "RenderedRequest",
            &version,
        )
        .unwrap_err()
        .to_string()
        .contains("expected doc-request"));

        let ambiguous = serde_json::json!({
            "RenderedRequest": [
                { "_docID": "doc-request" },
                { "_docID": "doc-request" }
            ]
        });
        assert!(unverified_exact_document_snapshot_from_data(
            &ambiguous,
            "RenderedRequest",
            &version,
        )
        .unwrap_err()
        .to_string()
        .contains("returned 2 documents"));
    }

    #[test]
    fn exact_snapshot_provenance_rejects_a_field_commit_cid() {
        let version = DocumentVersionRef::new("doc-request", "bafy-field");
        let field_commit = serde_json::json!([{
            "cid": "bafy-field",
            "docID": "doc-request",
            "fieldName": "request_json",
            "collectionVersionId": "bafy-schema-rendered-request"
        }]);
        let error = validate_composite_document_version_from_commits(
            &field_commit,
            "RenderedRequest",
            &version,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("not the requested composite document version"));

        let composite = serde_json::json!([{
            "cid": "bafy-field",
            "docID": "doc-request",
            "fieldName": "_C",
            "collectionVersionId": "bafy-schema-rendered-request"
        }]);
        assert_eq!(
            validate_composite_document_version_from_commits(
                &composite,
                "RenderedRequest",
                &version
            )
            .unwrap(),
            "bafy-schema-rendered-request"
        );

        let missing_schema_version = serde_json::json!([{
            "cid": "bafy-field",
            "docID": "doc-request",
            "fieldName": "_C"
        }]);
        assert!(validate_composite_document_version_from_commits(
            &missing_schema_version,
            "RenderedRequest",
            &version
        )
        .unwrap_err()
        .to_string()
        .contains("omitted collectionVersionId"));
    }

    #[test]
    fn field_version_follows_selected_composite_links() {
        let version = DocumentVersionRef::new("doc-request", "bafy-composite");
        let commits = serde_json::json!([{
            "cid": "bafy-composite",
            "docID": "doc-request",
            "fieldName": "_C",
            "collectionVersionId": "bafy-schema-rendered-request",
            "links": [
                { "cid": "bafy-request-json", "fieldName": "request_json" },
                { "cid": "bafy-created-at", "fieldName": "created_at" }
            ]
        }]);

        let field = document_field_version_ref_from_commits(
            &commits,
            "RenderedRequest",
            &version,
            "request_json",
        )
        .unwrap();
        assert_eq!(field.document_version, version);
        assert_eq!(field.field_name, "request_json");
        assert_eq!(field.field_commit_cid, "bafy-request-json");

        let query =
            document_field_version_query("RenderedRequest", &version, "request_json").unwrap();
        assert!(query.contains("cid: [\"bafy-composite\"]"));
        assert!(query.contains("docID: [\"doc-request\"]"));
        assert!(query.contains("collectionVersionId"));
        assert!(query.contains("links { cid fieldName }"));
        assert!(!query.contains("limit"));
    }

    #[test]
    fn field_version_rejects_ambiguous_or_unrelated_commit_rows() {
        let version = DocumentVersionRef::new("doc-request", "bafy-composite");
        let duplicate_links = serde_json::json!([{
            "cid": "bafy-composite",
            "docID": "doc-request",
            "fieldName": "_C",
            "collectionVersionId": "bafy-schema-rendered-request",
            "links": [
                { "cid": "bafy-one", "fieldName": "request_json" },
                { "cid": "bafy-two", "fieldName": "request_json" }
            ]
        }]);
        assert!(document_field_version_ref_from_commits(
            &duplicate_links,
            "RenderedRequest",
            &version,
            "request_json"
        )
        .unwrap_err()
        .to_string()
        .contains("2 request_json field links"));

        let wrong_composite = serde_json::json!([{
            "cid": "bafy-other",
            "docID": "doc-request",
            "fieldName": "_C",
            "collectionVersionId": "bafy-schema-rendered-request",
            "links": [{ "cid": "bafy-value", "fieldName": "request_json" }]
        }]);
        assert!(document_field_version_ref_from_commits(
            &wrong_composite,
            "RenderedRequest",
            &version,
            "request_json"
        )
        .unwrap_err()
        .to_string()
        .contains("not the requested composite document version"));
    }

    fn fact(collection: &str, logical_id: &str) -> ConfigFactRef {
        ConfigFactRef::new(
            collection,
            logical_id,
            format!("bafy-schema-{collection}"),
            SignedDocumentVersionRef::new(
                DocumentVersionRef::new(format!("doc-{logical_id}"), format!("bafy-{logical_id}")),
                "did:key:zSigner",
            ),
        )
    }

    fn provenance() -> ResolvedBehaviorConfigProvenance {
        ResolvedBehaviorConfigProvenance {
            principal: fact("AgentPrincipal", "did:key:zAgent"),
            behavior: fact("AgentBehavior", "default"),
            inference_backend: fact("InferenceBackend", "backend"),
            inference_profile: fact("InferenceProfile", "profile"),
            tool_selection: Some(fact("ToolSelection", "tools")),
            datastore_tool_surfaces: Vec::new(),
            skills: vec![fact("Skill", "alpha"), fact("Skill", "zeta")],
            resolution_algorithm_version: 1,
        }
    }

    #[test]
    fn resolved_behavior_config_provenance_accepts_canonical_exact_facts() {
        provenance()
            .validate_for_behavior("default", "did:key:zAgent")
            .unwrap();
    }

    #[test]
    fn resolved_behavior_config_provenance_rejects_duplicate_or_unsorted_skills() {
        let mut duplicate = provenance();
        duplicate.skills[1] = duplicate.skills[0].clone();
        assert!(duplicate
            .validate_for_behavior("default", "did:key:zAgent")
            .unwrap_err()
            .to_string()
            .contains("unique and canonically ordered"));

        let mut unsorted = provenance();
        unsorted.skills.reverse();
        assert!(unsorted
            .validate_for_behavior("default", "did:key:zAgent")
            .unwrap_err()
            .to_string()
            .contains("unique and canonically ordered"));
    }

    #[test]
    fn resolved_behavior_config_provenance_rejects_duplicate_or_unsorted_datastore_surfaces() {
        let mut duplicate = provenance();
        duplicate.datastore_tool_surfaces = vec![
            fact("DatastoreToolSurface", "alpha"),
            fact("DatastoreToolSurface", "alpha"),
        ];
        assert!(duplicate
            .validate_for_behavior("default", "did:key:zAgent")
            .unwrap_err()
            .to_string()
            .contains("unique and canonically ordered"));

        let mut unsorted = provenance();
        unsorted.datastore_tool_surfaces = vec![
            fact("DatastoreToolSurface", "zeta"),
            fact("DatastoreToolSurface", "alpha"),
        ];
        assert!(unsorted
            .validate_for_behavior("default", "did:key:zAgent")
            .unwrap_err()
            .to_string()
            .contains("unique and canonically ordered"));
    }
}
