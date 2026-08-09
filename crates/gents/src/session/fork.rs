use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest};
use gents_protocol::graphql::GraphqlRequestOptions;
use identity::Did;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::retry::log_mutation_timing;
use super::{CompactionFactRef, CompactionSourceManifest};
use crate::graphql::escape_graphql_string;
use crate::lifecycle::active_runtime_lifecycle_state_graphql_list;
use crate::retry::{
    defradb_conflict_retry_backoff, is_defradb_transaction_conflict_text,
    DEFRA_DB_CONFLICT_MAX_RETRIES,
};
use crate::AuthenticatedGraphql;

#[derive(Debug, Clone)]
pub struct GraphqlExecuteResponse {
    pub data: Option<Value>,
    pub errors: Vec<Value>,
}

impl GraphqlExecuteResponse {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    fn from_http_value(value: Value) -> Self {
        let data = value.get("data").cloned();
        let errors = value
            .get("errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Self { data, errors }
    }

    fn from_embedded(response: defra_node::QueryResponse) -> Self {
        let errors = response
            .errors
            .into_iter()
            .map(|error| {
                serde_json::to_value(error)
                    .unwrap_or_else(|_| Value::String("GraphQL error".to_string()))
            })
            .collect();
        Self {
            data: response.data,
            errors,
        }
    }
}

#[async_trait]
pub trait GraphqlExecutor: Send + Sync {
    async fn execute_graphql(&self, query: &str) -> Result<GraphqlExecuteResponse>;

    /// Execute a mutation only if the current composite head still equals the
    /// exact signed version accepted by the caller. The head read, mutation,
    /// and commit share one DefraDB transaction so a same-phase concurrent
    /// write cannot authorize evidence for an older execution version.
    async fn execute_graphql_exact_head_cas(
        &self,
        collection: &str,
        accepted: &crate::SignedDocumentVersionRef,
        mutation: &str,
        operation: &'static str,
    ) -> Result<GraphqlExecuteResponse>;

    async fn verified_signer_did(&self, cid: &str) -> Result<String>;

    fn node_identity_did(&self) -> Option<&str> {
        None
    }
}

#[async_trait]
impl GraphqlExecutor for EmbeddedNode {
    async fn execute_graphql(&self, query: &str) -> Result<GraphqlExecuteResponse> {
        let identity = EmbeddedNode::node_identity_did(self)
            .map(Did::new)
            .transpose()
            .context("parsing fork node query identity")?;
        Ok(GraphqlExecuteResponse::from_embedded(
            self.execute_request_with_retry(
                QueryRequest::new(query).with_identity(identity),
                ExecuteRetryPolicy::default(),
            )
            .await,
        ))
    }

    async fn verified_signer_did(&self, cid: &str) -> Result<String> {
        self.verified_block_signer_did(cid).await
    }

    async fn execute_graphql_exact_head_cas(
        &self,
        collection: &str,
        accepted: &crate::SignedDocumentVersionRef,
        mutation: &str,
        operation: &'static str,
    ) -> Result<GraphqlExecuteResponse> {
        let identity = EmbeddedNode::node_identity_did(self)
            .map(Did::new)
            .transpose()
            .context("parsing fork transaction query identity")?;
        for attempt in 0..=DEFRA_DB_CONFLICT_MAX_RETRIES {
            let txn =
                crate::config_client::ConfigApplyTxn::begin_local(self, identity.clone()).await?;
            match execute_fork_exact_head_cas_in_txn(
                &txn, collection, accepted, mutation, operation,
            )
            .await
            {
                Ok(response) => match txn.commit().await {
                    Ok(()) => return Ok(response),
                    Err(error)
                        if attempt < DEFRA_DB_CONFLICT_MAX_RETRIES
                            && is_defradb_transaction_conflict_text(&error.to_string()) =>
                    {
                        tokio::time::sleep(defradb_conflict_retry_backoff(attempt)).await;
                    }
                    Err(error) => return Err(error).context(operation),
                },
                Err(error) => {
                    if let Err(discard_error) = txn.discard().await {
                        tracing::warn!(%discard_error, operation, "discarding failed fork exact-head transaction failed");
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("bounded fork exact-head transaction loop returns")
    }

    fn node_identity_did(&self) -> Option<&str> {
        EmbeddedNode::node_identity_did(self)
    }
}

#[derive(Debug, Clone)]
pub struct HttpGraphqlExecutor {
    access: AuthenticatedGraphql,
    options: GraphqlRequestOptions,
}

impl HttpGraphqlExecutor {
    pub fn new(access: AuthenticatedGraphql) -> Self {
        Self {
            access,
            options: GraphqlRequestOptions::default(),
        }
    }

    pub fn with_options(access: AuthenticatedGraphql, options: GraphqlRequestOptions) -> Self {
        Self { access, options }
    }
}

#[derive(serde::Deserialize)]
struct SignedBlockBundle {
    cid: String,
    block: String,
    signature: String,
}

fn locally_verified_block_signer_did(
    cid: &str,
    block_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<String> {
    let block = defra_core::block::Block::from_dag_cbor(block_bytes)
        .with_context(|| format!("decoding DAG-CBOR block {cid}"))?;
    if block.generate_cid()?.to_string() != cid {
        anyhow::bail!("remote block bytes do not hash to requested commit {cid}");
    }
    let signature_cid = block
        .signature
        .ok_or_else(|| anyhow::anyhow!("remote commit {cid} has no signature link"))?;
    let signature = defra_core::block::Signature::from_dag_cbor(signature_bytes)
        .with_context(|| format!("decoding signature block for {cid}"))?;
    if signature.generate_cid()? != signature_cid {
        anyhow::bail!("remote signature bytes do not hash to {signature_cid}");
    }
    let public_key_hex = std::str::from_utf8(&signature.header.identity)
        .with_context(|| format!("decoding signature identity for {cid}"))?;
    let key_type = match signature.header.sig_type {
        defra_core::block::SignatureType::EdDSA => crypto::KeyType::Ed25519,
        defra_core::block::SignatureType::ES256K => crypto::KeyType::Secp256k1,
        defra_core::block::SignatureType::ES256 => crypto::KeyType::Secp256r1,
        defra_core::block::SignatureType::BLS => crypto::KeyType::Bls12381,
    };
    let public_key = crypto::public_key_from_string(key_type, public_key_hex)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("decoding signer key for remote commit {cid}"))?;
    let mut unsigned = block;
    unsigned.signature = None;
    let signed_bytes = unsigned
        .to_dag_cbor()
        .with_context(|| format!("encoding unsigned block {cid} for verification"))?;
    if !public_key
        .verify(&signed_bytes, &signature.value)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("verifying remote commit {cid} signature locally"))?
    {
        anyhow::bail!("remote commit {cid} signature verification failed");
    }
    public_key
        .did()
        .map_err(anyhow::Error::from)
        .with_context(|| format!("deriving locally verified signer DID for remote commit {cid}"))
}

#[cfg(test)]
mod local_signature_tests {
    use crypto::PrivateKey;
    use defra_core::block::{
        Block, CrdtDelta, LwwDeltaPayload, Signature, SignatureHeader, SignatureType,
    };

    use super::locally_verified_block_signer_did;

    fn signed_bundle() -> (String, String, Vec<u8>, Vec<u8>) {
        let private_key = crypto::generate_ed25519().expect("generate test signing key");
        let public_key = private_key.public_key();
        let signer_did = public_key.did().expect("derive signer DID");
        let mut block = Block {
            delta: CrdtDelta::Lww(LwwDeltaPayload {
                field_name: "content".to_string(),
                schema_version_id: "agent-request-v1".to_string(),
                priority: 1,
                data: b"signed request".to_vec(),
            }),
            heads: None,
            links: None,
            encryption: None,
            signature: None,
        };
        let signature = Signature::new(
            SignatureHeader::new(
                SignatureType::EdDSA,
                public_key.to_hex_string().into_bytes(),
            ),
            private_key
                .sign(&block.to_dag_cbor().expect("encode unsigned block"))
                .expect("sign block"),
        );
        block.signature = Some(signature.generate_cid().expect("hash signature"));
        let cid = block.generate_cid().expect("hash signed block").to_string();
        (
            cid,
            signer_did,
            block.to_dag_cbor().expect("encode signed block"),
            signature.to_dag_cbor().expect("encode signature"),
        )
    }

    #[test]
    fn remote_signed_material_is_verified_locally() {
        let (cid, signer_did, block, signature) = signed_bundle();
        assert_eq!(
            locally_verified_block_signer_did(&cid, &block, &signature)
                .expect("valid bundle should verify"),
            signer_did
        );
    }

    #[test]
    fn remote_signed_material_rejects_cid_rebinding_and_signature_tampering() {
        let (cid, _, block, mut signature) = signed_bundle();
        let rebound = locally_verified_block_signer_did("bafy-rebound", &block, &signature)
            .expect_err("rebound CID must fail");
        assert!(rebound.to_string().contains("do not hash"), "{rebound:#}");

        let last = signature.last_mut().expect("signature bytes are non-empty");
        *last ^= 1;
        let tampered = locally_verified_block_signer_did(&cid, &block, &signature)
            .expect_err("tampered signature must fail");
        assert!(
            tampered.to_string().contains("signature") || tampered.to_string().contains("DAG-CBOR"),
            "{tampered:#}"
        );
    }
}

#[async_trait]
impl GraphqlExecutor for HttpGraphqlExecutor {
    async fn execute_graphql(&self, query: &str) -> Result<GraphqlExecuteResponse> {
        let value = self.access.execute(query, self.options).await?;
        Ok(GraphqlExecuteResponse::from_http_value(value))
    }

    async fn verified_signer_did(&self, cid: &str) -> Result<String> {
        let api_base = crate::config_client::graphql_api_base(self.access.endpoint())?;
        let mut signed_url = reqwest::Url::parse(&format!("{api_base}/block/signed"))?;
        signed_url.query_pairs_mut().append_pair("cid", cid);
        let response = self.access.get(signed_url).await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            anyhow::bail!(
                "loading signed material for remote commit {cid} failed (HTTP {status}): {}",
                String::from_utf8_lossy(&body)
            );
        }
        let bundle: SignedBlockBundle = serde_json::from_slice(&body)
            .with_context(|| format!("decoding signed material for remote commit {cid}"))?;
        if bundle.cid != cid {
            anyhow::bail!("remote signed-material response rebound commit {cid}");
        }
        let decoder = base64::engine::general_purpose::STANDARD;
        let block_bytes = decoder
            .decode(bundle.block)
            .with_context(|| format!("decoding remote block {cid}"))?;
        let signature_bytes = decoder
            .decode(bundle.signature)
            .with_context(|| format!("decoding remote signature block for {cid}"))?;
        locally_verified_block_signer_did(cid, &block_bytes, &signature_bytes)
    }

    async fn execute_graphql_exact_head_cas(
        &self,
        collection: &str,
        accepted: &crate::SignedDocumentVersionRef,
        mutation: &str,
        operation: &'static str,
    ) -> Result<GraphqlExecuteResponse> {
        let access = crate::config_client::ConfigAccess::Graphql(self.access.clone());
        for attempt in 0..=DEFRA_DB_CONFLICT_MAX_RETRIES {
            let txn = access.begin_apply_txn().await?;
            match execute_fork_exact_head_cas_in_txn(
                &txn, collection, accepted, mutation, operation,
            )
            .await
            {
                Ok(response) => match txn.commit().await {
                    Ok(()) => return Ok(response),
                    Err(error)
                        if attempt < DEFRA_DB_CONFLICT_MAX_RETRIES
                            && is_defradb_transaction_conflict_text(&error.to_string()) =>
                    {
                        tokio::time::sleep(defradb_conflict_retry_backoff(attempt)).await;
                    }
                    Err(error) => return Err(error).context(operation),
                },
                Err(error) => {
                    if let Err(discard_error) = txn.discard().await {
                        tracing::warn!(%discard_error, operation, "discarding failed remote fork exact-head transaction failed");
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("bounded remote fork exact-head transaction loop returns")
    }
}

async fn execute_fork_exact_head_cas_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    accepted: &crate::SignedDocumentVersionRef,
    mutation: &str,
    operation: &'static str,
) -> Result<GraphqlExecuteResponse> {
    let current_query =
        crate::document_version::current_composite_metadata_query(&accepted.version.doc_id);
    let current_value = txn.execute(&current_query).await?;
    let current_response = GraphqlExecuteResponse::from_http_value(current_value);
    if current_response.has_errors() {
        anyhow::bail!(
            "{operation} exact-head query failed: {}",
            render_graphql_errors(&current_response)
        );
    }
    let commits = current_response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let current =
        crate::document_version::unverified_current_document_version_metadata_from_commits(
            &commits,
            collection,
            &accepted.version.doc_id,
        )?;
    if current.version != accepted.version {
        anyhow::bail!(
            "{operation} exact source changed from {} to {} before terminal evidence binding",
            accepted.version.composite_commit_cid,
            current.version.composite_commit_cid
        );
    }
    let mutation_value = txn.execute(mutation).await?;
    let response = GraphqlExecuteResponse::from_http_value(mutation_value);
    if response.has_errors() {
        anyhow::bail!(
            "{operation} exact terminal mutation failed: {}",
            render_graphql_errors(&response)
        );
    }
    Ok(response)
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ForkCommitParent {
    cid: String,
    #[serde(rename = "fieldName")]
    field_name: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ForkCommitRow {
    cid: String,
    #[serde(default)]
    heads: Vec<ForkCommitParent>,
}

async fn exact_current_ref(
    executor: &(impl GraphqlExecutor + ?Sized),
    collection: &str,
    doc_id: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    let response = executor
        .execute_graphql(&format!(
            r#"{{ _commits(docID: ["{}"], filter: {{ fieldName: {{ _eq: "_C" }} }}) {{ cid heads {{ cid fieldName }} }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    if response.has_errors() {
        anyhow::bail!(
            "loading exact {collection} commit evidence failed: {}",
            render_graphql_errors(&response)
        );
    }
    let commits: Vec<ForkCommitRow> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("_commits"))
            .cloned()
            .unwrap_or_default(),
    )?;
    let nested = commits
        .iter()
        .flat_map(|commit| commit.heads.iter())
        .filter(|head| head.field_name.as_deref() == Some("_C"))
        .map(|head| head.cid.as_str())
        .collect::<HashSet<_>>();
    let current = commits
        .iter()
        .filter(|commit| !nested.contains(commit.cid.as_str()))
        .collect::<Vec<_>>();
    let [current] = current.as_slice() else {
        anyhow::bail!(
            "{collection} {doc_id} has {} current composite heads",
            current.len()
        );
    };
    let signer = executor.verified_signer_did(&current.cid).await?;
    Ok(crate::SignedDocumentVersionRef::new(
        crate::DocumentVersionRef::new(doc_id, &current.cid),
        signer,
    ))
}

async fn exact_snapshot(
    executor: &(impl GraphqlExecutor + ?Sized),
    collection: &str,
    source: &crate::SignedDocumentVersionRef,
    fields: &str,
) -> Result<Value> {
    let verified = executor
        .verified_signer_did(&source.version.composite_commit_cid)
        .await?;
    if verified != source.signer_did {
        anyhow::bail!("{collection} exact source signer does not verify");
    }
    let response = executor
        .execute_graphql(&format!(
            r#"{{ {collection}(cid: ["{}"]) {{ _docID {fields} }} }}"#,
            escape_graphql_string(&source.version.composite_commit_cid)
        ))
        .await?;
    if response.has_errors() {
        anyhow::bail!(
            "loading exact {collection} source failed: {}",
            render_graphql_errors(&response)
        );
    }
    let rows = graphql_rows(&response, collection);
    match rows.as_slice() {
        [row]
            if row.get("_docID").and_then(Value::as_str)
                == Some(source.version.doc_id.as_str()) =>
        {
            Ok(row.clone())
        }
        rows => anyhow::bail!(
            "{collection} exact source reconstructed {} rows or a different document",
            rows.len()
        ),
    }
}

async fn exact_collection_version_id(
    executor: &(impl GraphqlExecutor + ?Sized),
    collection: &str,
    source: &crate::SignedDocumentVersionRef,
) -> Result<String> {
    let response = executor
        .execute_graphql(&format!(
            r#"{{ _commits(cid: ["{}"], docID: ["{}"]) {{ cid docID fieldName collectionVersionId }} }}"#,
            escape_graphql_string(&source.version.composite_commit_cid),
            escape_graphql_string(&source.version.doc_id),
        ))
        .await?;
    if response.has_errors() {
        anyhow::bail!(
            "loading exact {collection} schema identity failed: {}",
            render_graphql_errors(&response)
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("exact {collection} schema query returned no rows"))?;
    let [row] = rows.as_slice() else {
        anyhow::bail!(
            "exact {collection} schema query returned {} commits, expected one",
            rows.len()
        );
    };
    if row.get("cid").and_then(Value::as_str) != Some(source.version.composite_commit_cid.as_str())
        || row.get("docID").and_then(Value::as_str) != Some(source.version.doc_id.as_str())
        || row.get("fieldName").and_then(Value::as_str) != Some("_C")
    {
        anyhow::bail!("exact {collection} schema query did not return the pinned composite commit");
    }
    row.get("collectionVersionId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("exact {collection} source omitted collectionVersionId"))
}

fn fork_source_fields(source: &crate::SignedDocumentVersionRef) -> String {
    format!(
        r#"fork_source_doc_id: "{}",
            fork_source_composite_commit_cid: "{}",
            fork_source_signer_did: "{}","#,
        escape_graphql_string(&source.version.doc_id),
        escape_graphql_string(&source.version.composite_commit_cid),
        escape_graphql_string(&source.signer_did),
    )
}

async fn verify_child_ref(
    executor: &(impl GraphqlExecutor + ?Sized),
    collection: &str,
    doc_id: &str,
    node_did: Option<&str>,
) -> Result<crate::SignedDocumentVersionRef> {
    let child = exact_current_ref(executor, collection, doc_id).await?;
    if node_did.is_some_and(|node_did| child.signer_did != node_did) {
        anyhow::bail!(
            "forked {collection} signer {} does not match node identity {}",
            child.signer_did,
            node_did.unwrap_or_default()
        );
    }
    Ok(child)
}

fn mutation_doc_id(response: &GraphqlExecuteResponse, field: &str) -> Result<String> {
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("{field} returned no mutation data"))?;
    // DefraDB currently exposes `create_Foo` in the schema but serializes the
    // response under its underlying `add_Foo` resolver name. Accept both
    // spellings while still requiring one physical result row.
    let add_field = field
        .strip_prefix("create_")
        .map(|collection| format!("add_{collection}"));
    let value = data
        .get(field)
        .or_else(|| add_field.as_deref().and_then(|field| data.get(field)))
        .ok_or_else(|| anyhow::anyhow!("{field} returned no mutation payload: data={}", data))?;
    if let Some(doc_id) = value.get("_docID").and_then(Value::as_str) {
        return Ok(doc_id.to_owned());
    }
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{field} returned an unknown mutation payload shape"))?;
    match rows.as_slice() {
        [row] => row
            .get("_docID")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("{field} returned no physical _docID")),
        rows => anyhow::bail!(
            "{field} returned {} physical rows; expected exactly one",
            rows.len()
        ),
    }
}

fn reject_logical_twins(rows: &[Value], field: &str, label: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for row in rows {
        let key = row
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("{label} row missing {field}"))?;
        if !seen.insert(key) {
            anyhow::bail!("{label} contains replicated logical twins for {field}={key}");
        }
    }
    Ok(())
}

async fn require_child_key_absent(
    executor: &(impl GraphqlExecutor + ?Sized),
    collection: &str,
    field: &str,
    value: &str,
) -> Result<()> {
    let response = executor
        .execute_graphql(&format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(value)
        ))
        .await?;
    if response.has_errors() {
        anyhow::bail!(
            "enumerating fork target {collection} failed: {}",
            render_graphql_errors(&response)
        );
    }
    let rows = graphql_rows(&response, collection);
    if !rows.is_empty() {
        anyhow::bail!(
            "fork target {collection} logical key has {} pre-existing physical rows",
            rows.len()
        );
    }
    Ok(())
}

async fn require_sole_child_key(
    executor: &(impl GraphqlExecutor + ?Sized),
    collection: &str,
    field: &str,
    value: &str,
    expected_doc_id: &str,
) -> Result<()> {
    let response = executor
        .execute_graphql(&format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(value)
        ))
        .await?;
    if response.has_errors() {
        anyhow::bail!(
            "re-enumerating fork target {collection} failed: {}",
            render_graphql_errors(&response)
        );
    }
    let rows = graphql_rows(&response, collection);
    match rows.as_slice() {
        [row]
            if row.get("_docID").and_then(Value::as_str) == Some(expected_doc_id) => Ok(()),
        rows => anyhow::bail!(
            "fork target {collection} logical key resolved to {} physical twins or another document",
            rows.len()
        ),
    }
}

fn optional_exact_ref(
    row: &Value,
    doc_field: &str,
    cid_field: &str,
    signer_field: &str,
    label: &str,
) -> Result<Option<crate::SignedDocumentVersionRef>> {
    let doc_id = row.get(doc_field).and_then(Value::as_str);
    let cid = row.get(cid_field).and_then(Value::as_str);
    let signer = row.get(signer_field).and_then(Value::as_str);
    match (doc_id, cid, signer) {
        (None, None, None) => Ok(None),
        (Some(doc_id), Some(cid), Some(signer))
            if !doc_id.trim().is_empty() && !cid.trim().is_empty() && !signer.trim().is_empty() =>
        {
            Ok(Some(crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(doc_id, cid),
                signer,
            )))
        }
        _ => anyhow::bail!("{label} exact source reference is partial or empty"),
    }
}

async fn attach_child_tool_fact(
    executor: &(impl GraphqlExecutor + ?Sized),
    child_call_doc_id: &str,
    kind: &str,
    fact: &crate::SignedDocumentVersionRef,
) -> Result<()> {
    let (doc_field, cid_field, signer_field) = match kind {
        "result" => (
            "result_doc_id",
            "result_composite_commit_cid",
            "result_signer_did",
        ),
        "approval" => (
            "approval_doc_id",
            "approval_composite_commit_cid",
            "approval_signer_did",
        ),
        _ => anyhow::bail!("unsupported child tool fact kind {kind}"),
    };
    let mutation = format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }}, {doc_field}: {{ _eq: null }}, {cid_field}: {{ _eq: null }}, {signer_field}: {{ _eq: null }} }}, input: {{ {doc_field}: "{}", {cid_field}: "{}", {signer_field}: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(child_call_doc_id),
        escape_graphql_string(&fact.version.doc_id),
        escape_graphql_string(&fact.version.composite_commit_cid),
        escape_graphql_string(&fact.signer_did),
    );
    let response =
        execute_mutation_with_retry(executor, &mutation, &format!("fork::attach_child_{kind}"))
            .await?;
    let updated = response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentToolCall"))
        .is_some_and(|value| match value {
            Value::Object(row) => row.get("_docID").is_some(),
            Value::Array(rows) => rows.len() == 1 && rows[0].get("_docID").is_some(),
            _ => false,
        });
    if !updated {
        let query = format!(
            r#"query {{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID {doc_field} {cid_field} {signer_field} }} }}"#,
            escape_graphql_string(child_call_doc_id),
        );
        let current = executor.execute_graphql(&query).await?;
        if current.has_errors() {
            anyhow::bail!(
                "loading child call after {kind} attachment CAS failed: {}",
                render_graphql_errors(&current)
            );
        }
        let rows = current
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let already_attached = matches!(rows.as_slice(), [row]
            if row.get(doc_field).and_then(Value::as_str) == Some(fact.version.doc_id.as_str())
                && row.get(cid_field).and_then(Value::as_str)
                    == Some(fact.version.composite_commit_cid.as_str())
                && row.get(signer_field).and_then(Value::as_str)
                    == Some(fact.signer_did.as_str()));
        if !already_attached {
            anyhow::bail!(
                "attaching child {kind} did not update one empty edge or match the exact existing fact"
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ForkParentConversation {
    behavior_id: Option<String>,
    agent_did: Option<String>,
    agent_name: Option<String>,
    requester_did: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ForkParams<'a> {
    pub source_session_id: &'a str,
    pub fork_at_user_turn: u32,
    pub caller_agent_did: &'a str,
    pub target_behavior_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub session_id: String,
    pub copied_messages: u32,
    pub copied_tool_calls: u32,
    pub copied_tool_results: u32,
    pub copied_tool_approvals: u32,
    pub copied_compaction_entries: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("fork source not found: session_id={0}")]
    ForkSourceNotFound(String),
    #[error("fork source's agent_did does not match caller")]
    ForkNotSameAgent,
    #[error("fork source has an active runtime AgentRequest and is busy")]
    ForkSourceBusy,
    #[error("fork_at_user_turn={0} is out of range (parent has only {1} user messages)")]
    ForkAtUserTurnOutOfRange(u32, u32),
    #[error("target behavior not found: {0}")]
    ForkBehaviorNotFound(String),
    #[error("target behavior {0} is not owned by principal {1}")]
    ForkBehaviorNotOwnedByPrincipal(String, String),
    #[error("fork copy step failed: {0}")]
    ForkCopyFailed(#[from] anyhow::Error),
}

async fn load_parent_conversation(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
) -> Result<Option<ForkParentConversation>> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }}
                }}
            ) {{
                _docID
                behavior_id
                agent_did
                agent_name
                requester_did
            }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "loading conversation document for session_id={}: {}",
            source_session_id,
            render_graphql_errors(&resp)
        );
    }

    let rows = graphql_rows(&resp, "AgentConversation");
    let row = match rows.as_slice() {
        [] => return Ok(None),
        [row] => row,
        rows => anyhow::bail!(
            "source session resolves to {} physical AgentConversation twins",
            rows.len()
        ),
    };
    Ok(Some(ForkParentConversation {
        behavior_id: row
            .get("behavior_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        agent_did: row
            .get("agent_did")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        agent_name: row
            .get("agent_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        requester_did: row
            .get("requester_did")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }))
}

async fn verify_source_idle(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
) -> Result<bool> {
    let escaped = escape_graphql_string(source_session_id);
    let active_runtime_states = active_runtime_lifecycle_state_graphql_list();
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }},
                    lifecycle_state: {{ _in: {active_runtime_states} }}
                }},
                limit: 1
            ) {{ request_id }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "verify_source_idle query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentRequest");
    Ok(rows.is_empty())
}

pub async fn fork(node: &EmbeddedNode, params: ForkParams<'_>) -> Result<ForkOutcome, ForkError> {
    if node.node_identity_did().is_none() {
        return Err(ForkError::ForkCopyFailed(anyhow::anyhow!(
            "session fork requires a signed embedded DefraDB node before any document read or write"
        )));
    }
    fork_with_executor(node, params).await
}

pub async fn fork_via_http(
    access: AuthenticatedGraphql,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let executor = HttpGraphqlExecutor::new(access);
    fork_with_executor(&executor, params).await
}

async fn fork_with_executor(
    executor: &(impl GraphqlExecutor + ?Sized),
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let parent = load_parent_conversation(executor, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
        .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_string()))?;

    let parent_agent_did = parent.agent_did.as_deref().unwrap_or("");
    if parent_agent_did.is_empty() {
        return Err(ForkError::ForkSourceNotFound(
            params.source_session_id.to_string(),
        ));
    }
    if parent_agent_did != params.caller_agent_did {
        return Err(ForkError::ForkNotSameAgent);
    }
    let expected_node_did = executor.node_identity_did();

    if !verify_source_idle(executor, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
    {
        return Err(ForkError::ForkSourceBusy);
    }

    let (cut_seq, cut_ts) =
        match compute_cut(executor, params.source_session_id, params.fork_at_user_turn)
            .await
            .map_err(ForkError::ForkCopyFailed)?
        {
            Ok((seq, ts)) => (seq, ts),
            Err(total_user_msgs) => {
                return Err(ForkError::ForkAtUserTurnOutOfRange(
                    params.fork_at_user_turn,
                    total_user_msgs,
                ));
            }
        };

    let resolved_behavior_id = if let Some(target) = params.target_behavior_id {
        if let Some(err) = resolve_target_behavior(executor, target, parent_agent_did)
            .await
            .map_err(ForkError::ForkCopyFailed)?
        {
            return Err(err);
        }
        target.to_string()
    } else {
        parent.behavior_id.clone().unwrap_or_default()
    };

    let child_session_id = uuid::Uuid::new_v4().to_string();
    let parent_agent_name = parent.agent_name.as_deref().unwrap_or("");
    let (child_conversation_doc_id, node_did) = create_child_session_and_conversation(
        executor,
        &child_session_id,
        &resolved_behavior_id,
        params.source_session_id,
        params.fork_at_user_turn,
        parent_agent_did,
        parent_agent_name,
        parent.requester_did.as_deref(),
        expected_node_did,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_messages = copy_messages(
        executor,
        params.source_session_id,
        &child_session_id,
        parent_agent_did,
        parent.requester_did.as_deref(),
        &node_did,
        cut_seq,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_tool_calls = copy_tool_calls(
        executor,
        params.source_session_id,
        &child_session_id,
        parent_agent_did,
        parent.requester_did.as_deref(),
        &node_did,
        cut_seq,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    // Approval-denied omissions and their verdict must bind the same exact
    // held execution.  Copy approvals before terminal evidence so the fork
    // planner can defer that one attachment until the terminal compare; all
    // other approvals are attached while the child is still non-terminal.
    let (copied_tool_approvals, deferred_tool_approvals) = copy_tool_approvals(
        executor,
        &child_session_id,
        parent_agent_did,
        parent.requester_did.as_deref(),
        &node_did,
        &copied_tool_calls,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_tool_results = copy_tool_results(
        executor,
        &child_session_id,
        parent_agent_did,
        parent.requester_did.as_deref(),
        &child_conversation_doc_id,
        &node_did,
        &copied_tool_calls,
        &deferred_tool_approvals,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_compaction_entries = copy_compaction_entries(
        executor,
        params.source_session_id,
        &child_session_id,
        parent_agent_did,
        parent.requester_did.as_deref(),
        &node_did,
        &resolved_behavior_id,
        &copied_messages,
        &cut_ts,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    Ok(ForkOutcome {
        session_id: child_session_id,
        copied_messages: copied_messages.len() as u32,
        copied_tool_calls: copied_tool_calls.len() as u32,
        copied_tool_results,
        copied_tool_approvals,
        copied_compaction_entries,
    })
}

async fn resolve_target_behavior(
    executor: &(impl GraphqlExecutor + ?Sized),
    target_behavior_id: &str,
    parent_agent_did: &str,
) -> Result<Option<ForkError>> {
    let escaped = escape_graphql_string(target_behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(filter: {{ behavior_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "resolve_target_behavior query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentBehavior");
    if rows.is_empty() {
        return Ok(Some(ForkError::ForkBehaviorNotFound(
            target_behavior_id.to_string(),
        )));
    }
    let behavior_did = rows[0]
        .get("agent_did")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if behavior_did != parent_agent_did {
        return Ok(Some(ForkError::ForkBehaviorNotOwnedByPrincipal(
            target_behavior_id.to_string(),
            parent_agent_did.to_string(),
        )));
    }
    Ok(None)
}

async fn compute_cut(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    fork_at_user_turn: u32,
) -> Result<std::result::Result<(u32, String), u32>> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }},
                    role: {{ _eq: "user" }}
                }},
                order: {{ sequence: ASC }}
            ) {{ sequence timestamp }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!("compute_cut query failed: {}", render_graphql_errors(&resp));
    }
    let rows = graphql_rows(&resp, "AgentMessage");
    let total_user_msgs = rows.len() as u32;
    if fork_at_user_turn > total_user_msgs {
        return Ok(Err(total_user_msgs));
    }
    if fork_at_user_turn == total_user_msgs {
        return compute_end_cut(executor, source_session_id).await.map(Ok);
    }
    let row = &rows[fork_at_user_turn as usize];
    let seq = row
        .get("sequence")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("sequence missing"))? as u32;
    let ts = row
        .get("timestamp")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("timestamp missing"))?
        .to_string();
    Ok(Ok((seq, ts)))
}

async fn compute_end_cut(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
) -> Result<(u32, String)> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "compute_end_cut query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let max_sequence = graphql_rows(&resp, "AgentMessage")
        .first()
        .and_then(|row| row.get("sequence"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cut_seq = u32::try_from(max_sequence.saturating_add(1))
        .context("message sequence exceeds u32 during fork end cut")?;
    Ok((cut_seq, "9999-12-31T23:59:59Z".to_string()))
}

#[derive(Debug, Clone)]
struct ForkedMessage {
    source: crate::SignedDocumentVersionRef,
    source_collection_version_id: String,
    child: crate::SignedDocumentVersionRef,
    child_collection_version_id: String,
    sequence: u32,
}

async fn copy_messages(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    node_did: &str,
    cut_seq: u32,
) -> Result<Vec<ForkedMessage>> {
    let escaped_source = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    sequence: {{ _lt: {cut_seq} }}
                }},
                order: {{ sequence: ASC }}
            ) {{ _docID message_key }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_messages query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentMessage");
    reject_logical_twins(&rows, "message_key", "fork source AgentMessage")?;
    let mut copied = Vec::with_capacity(rows.len());
    for row in &rows {
        let source_doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source AgentMessage missing _docID"))?;
        let source_ref = exact_current_ref(executor, "AgentMessage", source_doc_id).await?;
        let source_collection_version_id =
            exact_collection_version_id(executor, "AgentMessage", &source_ref).await?;
        let row = exact_snapshot(
            executor,
            "AgentMessage",
            &source_ref,
            "message_key session_id agent_did requester_did request_id request_doc_id sequence role content reasoning timestamp",
        )
        .await?;
        if row.get("session_id").and_then(Value::as_str) != Some(source_session_id) {
            anyhow::bail!("exact AgentMessage source belongs to a different session");
        }
        let sequence = row
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("sequence missing"))?;
        let role = row.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = row.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let reasoning = row.get("reasoning").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = row.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let request_id = row.get("request_id").and_then(Value::as_str);
        let request_doc_id = row.get("request_doc_id").and_then(Value::as_str);
        let message_key = format!("{child_session_id}:{sequence}");
        require_child_key_absent(executor, "AgentMessage", "message_key", &message_key).await?;
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let source_fields = fork_source_fields(&source_ref);
        let mutation = format!(
            r#"mutation {{ create_AgentMessage(input: {{
                    message_key: "{message_key_escaped}",
                    session_id: "{child_session_escaped}",
                    agent_did: "{agent_did_escaped}",
                    {requester_did_field}
                    request_id: {request_id},
                    request_doc_id: {request_doc_id},
                    sequence: {sequence},
                    role: "{role_escaped}",
                    content: "{content_escaped}",
                    reasoning: "{reasoning_escaped}",
                    timestamp: "{timestamp_escaped}",
                    {source_fields}
                }}) {{ _docID }} }}"#,
            message_key_escaped = escape_graphql_string(&message_key),
            child_session_escaped = escape_graphql_string(child_session_id),
            agent_did_escaped = escape_graphql_string(agent_did),
            request_id = nullable_string_literal(request_id),
            request_doc_id = nullable_string_literal(request_doc_id),
            role_escaped = escape_graphql_string(role),
            content_escaped = escape_graphql_string(content),
            reasoning_escaped = escape_graphql_string(reasoning),
            timestamp_escaped = escape_graphql_string(timestamp),
        );
        let response =
            execute_mutation_with_retry(executor, &mutation, "fork::copy_message").await?;
        let doc_id = mutation_doc_id(&response, "create_AgentMessage")?;
        let child = verify_child_ref(executor, "AgentMessage", &doc_id, Some(node_did)).await?;
        let child_collection_version_id =
            exact_collection_version_id(executor, "AgentMessage", &child).await?;
        require_sole_child_key(
            executor,
            "AgentMessage",
            "message_key",
            &message_key,
            &doc_id,
        )
        .await?;
        copied.push(ForkedMessage {
            source: source_ref,
            source_collection_version_id,
            child,
            child_collection_version_id,
            sequence: u32::try_from(sequence).context("forked message sequence exceeds u32")?,
        });
    }
    Ok(copied)
}

#[derive(Debug, Clone)]
struct ForkedToolCall {
    source: crate::SignedDocumentVersionRef,
    child: crate::SignedDocumentVersionRef,
    source_row: Value,
    evidence: ForkedToolEvidence,
}

#[derive(Debug, Clone)]
enum ForkedToolEvidence {
    NonTerminal,
    Result {
        source: crate::SignedDocumentVersionRef,
        source_phase: crate::tool_call_lifecycle::ToolCallState,
        terminal_phase: crate::tool_call_lifecycle::ToolCallState,
    },
    Omission {
        source: crate::SignedDocumentVersionRef,
        accepted: crate::SignedDocumentVersionRef,
        source_phase: crate::tool_call_lifecycle::ToolCallState,
        terminal_phase: crate::tool_call_lifecycle::ToolCallState,
        reason: crate::tool_call_lifecycle::evidence::ToolOutputOmissionReason,
    },
}

impl ForkedToolEvidence {
    fn source_phase(&self) -> Option<crate::tool_call_lifecycle::ToolCallState> {
        match self {
            Self::NonTerminal => None,
            Self::Result { source_phase, .. } | Self::Omission { source_phase, .. } => {
                Some(*source_phase)
            }
        }
    }

    fn terminal_phase(&self) -> Option<crate::tool_call_lifecycle::ToolCallState> {
        match self {
            Self::NonTerminal => None,
            Self::Result { terminal_phase, .. } | Self::Omission { terminal_phase, .. } => {
                Some(*terminal_phase)
            }
        }
    }

    fn defers_approval_binding(&self) -> bool {
        matches!(
            self,
            Self::Omission {
                reason:
                    crate::tool_call_lifecycle::evidence::ToolOutputOmissionReason::ApprovalDenied,
                ..
            }
        )
    }
}

fn required_tool_phase(
    value: Option<&str>,
    label: &str,
) -> Result<crate::tool_call_lifecycle::ToolCallState> {
    let value = value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{label} omitted lifecycle_state"))?;
    crate::tool_call_lifecycle::ToolCallState::from_persisted(value)
        .ok_or_else(|| anyhow::anyhow!("{label} has unknown lifecycle_state {value}"))
}

fn persisted_omission_reason(
    value: &str,
) -> Result<crate::tool_call_lifecycle::evidence::ToolOutputOmissionReason> {
    use crate::tool_call_lifecycle::evidence::ToolOutputOmissionReason as Reason;
    match value {
        "preDispatchFailure" => Ok(Reason::PreDispatchFailure),
        "approvalDenied" => Ok(Reason::ApprovalDenied),
        "timedOut" => Ok(Reason::TimedOut),
        "cancelled" => Ok(Reason::Cancelled),
        "recoveryFailure" => Ok(Reason::RecoveryFailure),
        "executionLost" => Ok(Reason::ExecutionLost),
        "childDead" => Ok(Reason::ChildDead),
        "childSuperseded" => Ok(Reason::ChildSuperseded),
        _ => anyhow::bail!("fork source omission has unknown reason {value}"),
    }
}

fn exact_ref_from_fields(
    row: &Value,
    doc_field: &str,
    cid_field: &str,
    signer_field: &str,
    label: &str,
) -> Result<crate::SignedDocumentVersionRef> {
    optional_exact_ref(row, doc_field, cid_field, signer_field, label)?
        .ok_or_else(|| anyhow::anyhow!("{label} omitted its exact parent"))
}

fn same_optional_string(left: &Value, right: &Value, field: &str) -> bool {
    left.get(field).and_then(Value::as_str) == right.get(field).and_then(Value::as_str)
}

async fn validate_source_result_evidence(
    executor: &(impl GraphqlExecutor + ?Sized),
    call: &crate::SignedDocumentVersionRef,
    call_row: &Value,
    result: crate::SignedDocumentVersionRef,
    terminal_phase: crate::tool_call_lifecycle::ToolCallState,
) -> Result<ForkedToolEvidence> {
    use crate::tool_call_lifecycle::ToolCallState;
    if !matches!(
        terminal_phase,
        ToolCallState::Completed | ToolCallState::Failed
    ) {
        anyhow::bail!(
            "terminal AgentToolCall {} cannot bind AgentToolResult in phase {}",
            call.version.doc_id,
            terminal_phase.as_str()
        );
    }
    let result_row = exact_snapshot(
        executor,
        "AgentToolResult",
        &result,
        "result_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did agent_did requester_did session_id tool_name tool_input",
    )
    .await?;
    let accepted = exact_ref_from_fields(
        &result_row,
        "tool_call_doc_id",
        "tool_call_composite_commit_cid",
        "tool_call_signer_did",
        "fork source AgentToolResult parent",
    )?;
    if accepted.version.doc_id != call.version.doc_id {
        anyhow::bail!("source AgentToolResult points to a different physical tool call");
    }
    let accepted_row = exact_snapshot(
        executor,
        "AgentToolCall",
        &accepted,
        "tool_call_key agent_did requester_did session_id tool_name args lifecycle_state",
    )
    .await
    .context("verifying exact accepted execution for source AgentToolResult")?;
    let source_phase = required_tool_phase(
        accepted_row.get("lifecycle_state").and_then(Value::as_str),
        "source AgentToolResult parent",
    )?;
    if source_phase != ToolCallState::Running {
        anyhow::bail!(
            "source AgentToolResult parent is {}, expected running",
            source_phase.as_str()
        );
    }
    if result.signer_did != accepted.signer_did || call.signer_did != accepted.signer_did {
        anyhow::bail!("source AgentToolResult, accepted execution, and terminal signer differ");
    }
    for field in [
        "tool_call_key",
        "agent_did",
        "requester_did",
        "session_id",
        "tool_name",
    ] {
        if !same_optional_string(&result_row, &accepted_row, field)
            || !same_optional_string(call_row, &accepted_row, field)
        {
            anyhow::bail!("source AgentToolResult closure disagrees on {field}");
        }
    }
    if result_row.get("tool_input").and_then(Value::as_str)
        != accepted_row.get("args").and_then(Value::as_str)
    {
        anyhow::bail!("source AgentToolResult input does not match exact accepted execution args");
    }
    let result_key = result_row
        .get("result_key")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("source AgentToolResult omitted result_key"))?;
    if result_key != accepted.version.composite_commit_cid {
        anyhow::bail!("source AgentToolResult key does not pin its accepted execution CID");
    }
    Ok(ForkedToolEvidence::Result {
        source: result,
        source_phase,
        terminal_phase,
    })
}

async fn validate_source_omission_evidence(
    executor: &(impl GraphqlExecutor + ?Sized),
    call: &crate::SignedDocumentVersionRef,
    call_row: &Value,
    omission: crate::SignedDocumentVersionRef,
    terminal_phase: crate::tool_call_lifecycle::ToolCallState,
) -> Result<ForkedToolEvidence> {
    let omission_row = exact_snapshot(
        executor,
        "AgentToolOutputOmission",
        &omission,
        "omission_key tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did agent_did requester_did session_id source_phase terminal_phase reason detail created_at",
    )
    .await?;
    let accepted = exact_ref_from_fields(
        &omission_row,
        "tool_call_doc_id",
        "tool_call_composite_commit_cid",
        "tool_call_signer_did",
        "fork source AgentToolOutputOmission parent",
    )?;
    if accepted.version.doc_id != call.version.doc_id {
        anyhow::bail!("source AgentToolOutputOmission points to a different physical tool call");
    }
    let accepted_row = exact_snapshot(
        executor,
        "AgentToolCall",
        &accepted,
        "tool_call_key agent_did requester_did session_id lifecycle_state",
    )
    .await
    .context("verifying exact accepted execution for source omission")?;
    let source_phase = required_tool_phase(
        omission_row.get("source_phase").and_then(Value::as_str),
        "source AgentToolOutputOmission",
    )?;
    let accepted_phase = required_tool_phase(
        accepted_row.get("lifecycle_state").and_then(Value::as_str),
        "source AgentToolOutputOmission parent",
    )?;
    if accepted_phase != source_phase {
        anyhow::bail!("source omission phase does not match its exact accepted execution");
    }
    let recorded_terminal = required_tool_phase(
        omission_row.get("terminal_phase").and_then(Value::as_str),
        "source AgentToolOutputOmission",
    )?;
    if recorded_terminal != terminal_phase {
        anyhow::bail!("source omission terminal phase does not match terminal AgentToolCall");
    }
    let reason = persisted_omission_reason(
        omission_row
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    if !reason.allows(source_phase, terminal_phase) {
        anyhow::bail!("source omission reason is illegal for its recorded phase pair");
    }
    if omission.signer_did != accepted.signer_did || call.signer_did != accepted.signer_did {
        anyhow::bail!("source omission, accepted execution, and terminal signer differ");
    }
    for field in ["tool_call_key", "agent_did", "requester_did", "session_id"] {
        if !same_optional_string(&omission_row, &accepted_row, field)
            || !same_optional_string(call_row, &accepted_row, field)
        {
            anyhow::bail!("source AgentToolOutputOmission closure disagrees on {field}");
        }
    }
    if omission_row.get("omission_key").and_then(Value::as_str)
        != Some(accepted.version.composite_commit_cid.as_str())
    {
        anyhow::bail!("source omission key does not pin its exact accepted execution CID");
    }
    Ok(ForkedToolEvidence::Omission {
        source: omission,
        accepted,
        source_phase,
        terminal_phase,
        reason,
    })
}

async fn validate_source_tool_evidence(
    executor: &(impl GraphqlExecutor + ?Sized),
    call: &crate::SignedDocumentVersionRef,
    row: &Value,
) -> Result<ForkedToolEvidence> {
    let phase = required_tool_phase(
        row.get("lifecycle_state").and_then(Value::as_str),
        "source AgentToolCall",
    )?;
    let result = optional_exact_ref(
        row,
        "result_doc_id",
        "result_composite_commit_cid",
        "result_signer_did",
        "fork source AgentToolResult",
    )?;
    let omission = optional_exact_ref(
        row,
        "omission_doc_id",
        "omission_composite_commit_cid",
        "omission_signer_did",
        "fork source AgentToolOutputOmission",
    )?;
    if phase.is_terminal() {
        match (result, omission) {
            (Some(result), None) => {
                validate_source_result_evidence(executor, call, row, result, phase).await
            }
            (None, Some(omission)) => {
                validate_source_omission_evidence(executor, call, row, omission, phase).await
            }
            (None, None) => anyhow::bail!(
                "terminal source AgentToolCall {} has no exact result or omission",
                call.version.doc_id
            ),
            (Some(_), Some(_)) => anyhow::bail!(
                "terminal source AgentToolCall {} binds both result and omission",
                call.version.doc_id
            ),
        }
    } else if result.is_some() || omission.is_some() {
        anyhow::bail!("non-terminal source AgentToolCall binds terminal evidence");
    } else {
        Ok(ForkedToolEvidence::NonTerminal)
    }
}

async fn copy_tool_calls(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    node_did: &str,
    cut_seq: u32,
) -> Result<Vec<ForkedToolCall>> {
    let escaped_source = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    message_sequence: {{ _lt: {cut_seq} }}
                }},
                order: {{ message_sequence: ASC }}
            ) {{ _docID tool_call_key }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_tool_calls query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentToolCall");
    reject_logical_twins(&rows, "tool_call_key", "fork source AgentToolCall")?;
    let mut copied = Vec::with_capacity(rows.len());
    for row in &rows {
        let source_doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source AgentToolCall missing _docID"))?;
        let source = exact_current_ref(executor, "AgentToolCall", source_doc_id).await?;
        let row = exact_snapshot(
            executor,
            "AgentToolCall",
            &source,
            "tool_call_key request_id session_id agent_did requester_did message_sequence tool_name tool_call_id args result result_doc_id result_composite_commit_cid result_signer_did omission_doc_id omission_composite_commit_cid omission_signer_did approval_doc_id approval_composite_commit_cid approval_signer_did status lifecycle_state started_at deadline_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network latency_ms await_mode cancel_policy cancel_cause child_request_id",
        )
        .await?;
        if row.get("session_id").and_then(Value::as_str) != Some(source_session_id) {
            anyhow::bail!("exact AgentToolCall source belongs to a different session");
        }
        let message_sequence = row
            .get("message_sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("message_sequence missing"))?;
        let tool_name = row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_call_id = row
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = row.get("args").and_then(|v| v.as_str()).unwrap_or("");
        let result = row.get("result").and_then(|v| v.as_str()).unwrap_or("");
        let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let evidence = validate_source_tool_evidence(executor, &source, &row).await?;
        let lifecycle_state = row.get("lifecycle_state").and_then(|v| v.as_str());
        let started_at = row.get("started_at").and_then(|v| v.as_str()).unwrap_or("");
        let deadline_at = row.get("deadline_at").and_then(Value::as_str);
        let completed_at = row.get("completed_at").and_then(Value::as_str);
        let selected_service_id = row.get("selected_service_id").and_then(|v| v.as_str());
        let selected_tool_name = row.get("selected_tool_name").and_then(|v| v.as_str());
        let tool_failure_class = row.get("tool_failure_class").and_then(|v| v.as_str());
        let denial_reason = row.get("denial_reason").and_then(|v| v.as_str());
        let denied_argv = row.get("denied_argv").and_then(json_string_array);
        let denied_command = row.get("denied_command").and_then(|v| v.as_str());
        let denied_argument = row.get("denied_argument").and_then(|v| v.as_str());
        let denied_subcommand = row.get("denied_subcommand").and_then(|v| v.as_str());
        let denied_prefix = row.get("denied_prefix").and_then(json_string_array);
        let policy_mode = row.get("policy_mode").and_then(|v| v.as_str());
        let policy_network = row.get("policy_network").and_then(|v| v.as_str());
        let cancel_cause = row.get("cancel_cause").and_then(|v| v.as_str());
        let latency_ms = row.get("latency_ms").and_then(json_i64);
        let await_mode = row.get("await_mode").and_then(Value::as_str);
        let cancel_policy = row.get("cancel_policy").and_then(Value::as_str);
        let child_request_id = row.get("child_request_id").and_then(Value::as_str);
        let request_id = row.get("request_id").and_then(Value::as_str);
        let stage_phase = evidence.source_phase().map(|phase| phase.as_str());
        let staged_terminal = stage_phase.is_some();
        let create_result = if staged_terminal { "" } else { result };
        let create_status = if staged_terminal {
            "forkStaging"
        } else {
            status
        };
        let create_lifecycle_state = stage_phase.or(lifecycle_state);
        let staging_deadline = (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339();
        let create_deadline_at = if staged_terminal {
            Some(staging_deadline.as_str())
        } else {
            deadline_at
        };
        let create_started_at = if matches!(
            evidence.source_phase(),
            Some(
                crate::tool_call_lifecycle::ToolCallState::Pending
                    | crate::tool_call_lifecycle::ToolCallState::AwaitingApproval
            )
        ) {
            None
        } else {
            (!started_at.is_empty()).then_some(started_at)
        };
        let create_completed_at = (!staged_terminal).then_some(completed_at).flatten();
        let create_tool_failure_class = (!staged_terminal).then_some(tool_failure_class).flatten();
        let create_denial_reason = (!staged_terminal).then_some(denial_reason).flatten();
        let create_denied_argv = (!staged_terminal)
            .then_some(denied_argv.as_deref())
            .flatten();
        let create_denied_command = (!staged_terminal).then_some(denied_command).flatten();
        let create_denied_argument = (!staged_terminal).then_some(denied_argument).flatten();
        let create_denied_subcommand = (!staged_terminal).then_some(denied_subcommand).flatten();
        let create_denied_prefix = (!staged_terminal)
            .then_some(denied_prefix.as_deref())
            .flatten();
        let create_policy_mode = (!staged_terminal).then_some(policy_mode).flatten();
        let create_policy_network = (!staged_terminal).then_some(policy_network).flatten();
        let create_cancel_cause = (!staged_terminal).then_some(cancel_cause).flatten();
        let create_latency_ms = (!staged_terminal).then_some(latency_ms).flatten();
        let tool_call_key = format!("{child_session_id}:{tool_call_id}");
        require_child_key_absent(executor, "AgentToolCall", "tool_call_key", &tool_call_key)
            .await?;
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let source_fields = fork_source_fields(&source);
        let mutation = format!(
            r#"mutation {{ create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key_escaped}",
                    request_id: {request_id},
                    session_id: "{child_session_escaped}",
                    agent_did: "{agent_did_escaped}",
                    {requester_did_field}
                    message_sequence: {message_sequence},
                    tool_name: "{tool_name_escaped}",
                    tool_call_id: "{tool_call_id_escaped}",
                    args: "{args_escaped}",
                    result: "{result_escaped}",
                    status: "{status_escaped}",
                    lifecycle_state: {lifecycle_state},
                    started_at: {started_at},
                    deadline_at: {deadline_at},
                    completed_at: {completed_at},
                    selected_service_id: {selected_service_id},
                    selected_tool_name: {selected_tool_name},
                    tool_failure_class: {tool_failure_class},
                    denial_reason: {denial_reason},
                    denied_argv: {denied_argv},
                    denied_command: {denied_command},
                    denied_argument: {denied_argument},
                    denied_subcommand: {denied_subcommand},
                    denied_prefix: {denied_prefix},
                    policy_mode: {policy_mode},
                    policy_network: {policy_network},
                    cancel_cause: {cancel_cause},
                    latency_ms: {latency_ms},
                    await_mode: {await_mode},
                    cancel_policy: {cancel_policy},
                    child_request_id: {child_request_id},
                    {source_fields}
                }}) {{ _docID }} }}"#,
            tool_call_key_escaped = escape_graphql_string(&tool_call_key),
            request_id = nullable_string_literal(request_id),
            child_session_escaped = escape_graphql_string(child_session_id),
            agent_did_escaped = escape_graphql_string(agent_did),
            tool_name_escaped = escape_graphql_string(tool_name),
            tool_call_id_escaped = escape_graphql_string(tool_call_id),
            args_escaped = escape_graphql_string(args),
            result_escaped = escape_graphql_string(create_result),
            status_escaped = escape_graphql_string(create_status),
            lifecycle_state = nullable_string_literal(create_lifecycle_state),
            started_at = nullable_string_literal(create_started_at),
            deadline_at = nullable_string_literal(create_deadline_at),
            completed_at = nullable_string_literal(create_completed_at),
            selected_service_id = nullable_string_literal(selected_service_id),
            selected_tool_name = nullable_string_literal(selected_tool_name),
            tool_failure_class = nullable_string_literal(create_tool_failure_class),
            denial_reason = nullable_string_literal(create_denial_reason),
            denied_argv = nullable_string_array_literal(create_denied_argv),
            denied_command = nullable_string_literal(create_denied_command),
            denied_argument = nullable_string_literal(create_denied_argument),
            denied_subcommand = nullable_string_literal(create_denied_subcommand),
            denied_prefix = nullable_string_array_literal(create_denied_prefix),
            policy_mode = nullable_string_literal(create_policy_mode),
            policy_network = nullable_string_literal(create_policy_network),
            cancel_cause = nullable_string_literal(create_cancel_cause),
            latency_ms = nullable_i64_literal(create_latency_ms),
            await_mode = nullable_string_literal(await_mode),
            cancel_policy = nullable_string_literal(cancel_policy),
            child_request_id = nullable_string_literal(child_request_id),
        );
        let response =
            execute_mutation_with_retry(executor, &mutation, "fork::copy_tool_call").await?;
        let doc_id = mutation_doc_id(&response, "create_AgentToolCall")?;
        let child = verify_child_ref(executor, "AgentToolCall", &doc_id, Some(node_did)).await?;
        require_sole_child_key(
            executor,
            "AgentToolCall",
            "tool_call_key",
            &tool_call_key,
            &doc_id,
        )
        .await?;
        copied.push(ForkedToolCall {
            source,
            child,
            source_row: row,
            evidence,
        });
    }
    Ok(copied)
}

fn exact_fact_fields(kind: &str, fact: &crate::SignedDocumentVersionRef) -> Result<String> {
    let (doc, cid, signer) = match kind {
        "result" => (
            "result_doc_id",
            "result_composite_commit_cid",
            "result_signer_did",
        ),
        "omission" => (
            "omission_doc_id",
            "omission_composite_commit_cid",
            "omission_signer_did",
        ),
        "approval" => (
            "approval_doc_id",
            "approval_composite_commit_cid",
            "approval_signer_did",
        ),
        _ => anyhow::bail!("unsupported fork tool fact kind {kind}"),
    };
    Ok(format!(
        r#"{doc}: "{}", {cid}: "{}", {signer}: "{}","#,
        escape_graphql_string(&fact.version.doc_id),
        escape_graphql_string(&fact.version.composite_commit_cid),
        escape_graphql_string(&fact.signer_did),
    ))
}

async fn terminalize_forked_tool_call(
    executor: &(impl GraphqlExecutor + ?Sized),
    call: &ForkedToolCall,
    accepted: &crate::SignedDocumentVersionRef,
    kind: &str,
    evidence: &crate::SignedDocumentVersionRef,
    deferred_approval: Option<&crate::SignedDocumentVersionRef>,
    node_did: &str,
) -> Result<()> {
    let source_phase = call
        .evidence
        .source_phase()
        .ok_or_else(|| anyhow::anyhow!("non-terminal fork call cannot be terminalized"))?;
    let terminal_phase = call
        .evidence
        .terminal_phase()
        .ok_or_else(|| anyhow::anyhow!("fork terminal plan omitted terminal phase"))?;
    let current = exact_current_ref(executor, "AgentToolCall", &call.child.version.doc_id).await?;
    if &current != accepted {
        anyhow::bail!("fork child execution changed before exact terminal evidence was bound");
    }
    if evidence.signer_did != accepted.signer_did || accepted.signer_did != node_did {
        anyhow::bail!("fork child evidence, accepted execution, and node signer differ");
    }

    let row = &call.source_row;
    let evidence_fields = exact_fact_fields(kind, evidence)?;
    let approval_fields = deferred_approval
        .map(|approval| exact_fact_fields("approval", approval))
        .transpose()?
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{ update_AgentToolCall(
            filter: {{
                _docID: {{ _eq: "{doc_id}" }},
                lifecycle_state: {{ _eq: "{source_phase}" }},
                result_doc_id: {{ _eq: null }},
                omission_doc_id: {{ _eq: null }}
            }},
            input: {{
                result: "{result}",
                status: "{status}",
                lifecycle_state: "{terminal_phase}",
                started_at: {started_at},
                deadline_at: {deadline_at},
                completed_at: {completed_at},
                selected_service_id: {selected_service_id},
                selected_tool_name: {selected_tool_name},
                tool_failure_class: {tool_failure_class},
                denial_reason: {denial_reason},
                denied_argv: {denied_argv},
                denied_command: {denied_command},
                denied_argument: {denied_argument},
                denied_subcommand: {denied_subcommand},
                denied_prefix: {denied_prefix},
                policy_mode: {policy_mode},
                policy_network: {policy_network},
                cancel_cause: {cancel_cause},
                latency_ms: {latency_ms},
                {evidence_fields}
                {approval_fields}
                partial_output_tail: null,
                partial_output_seq: null
            }}
        ) {{ _docID }} }}"#,
        doc_id = escape_graphql_string(&call.child.version.doc_id),
        source_phase = source_phase.as_str(),
        terminal_phase = terminal_phase.as_str(),
        result = escape_graphql_string(
            row.get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        status = escape_graphql_string(
            row.get("status")
                .and_then(Value::as_str)
                .unwrap_or("completed")
        ),
        started_at = nullable_string_literal(row.get("started_at").and_then(Value::as_str)),
        deadline_at = nullable_string_literal(row.get("deadline_at").and_then(Value::as_str)),
        completed_at = nullable_string_literal(row.get("completed_at").and_then(Value::as_str)),
        selected_service_id =
            nullable_string_literal(row.get("selected_service_id").and_then(Value::as_str)),
        selected_tool_name =
            nullable_string_literal(row.get("selected_tool_name").and_then(Value::as_str)),
        tool_failure_class =
            nullable_string_literal(row.get("tool_failure_class").and_then(Value::as_str)),
        denial_reason = nullable_string_literal(row.get("denial_reason").and_then(Value::as_str)),
        denied_argv = nullable_string_array_literal(
            row.get("denied_argv")
                .and_then(json_string_array)
                .as_deref()
        ),
        denied_command = nullable_string_literal(row.get("denied_command").and_then(Value::as_str)),
        denied_argument =
            nullable_string_literal(row.get("denied_argument").and_then(Value::as_str)),
        denied_subcommand =
            nullable_string_literal(row.get("denied_subcommand").and_then(Value::as_str)),
        denied_prefix = nullable_string_array_literal(
            row.get("denied_prefix")
                .and_then(json_string_array)
                .as_deref()
        ),
        policy_mode = nullable_string_literal(row.get("policy_mode").and_then(Value::as_str)),
        policy_network = nullable_string_literal(row.get("policy_network").and_then(Value::as_str)),
        cancel_cause = nullable_string_literal(row.get("cancel_cause").and_then(Value::as_str)),
        latency_ms = nullable_i64_literal(row.get("latency_ms").and_then(json_i64)),
    );
    let response = executor
        .execute_graphql_exact_head_cas(
            "AgentToolCall",
            accepted,
            &mutation,
            "fork::terminalize_tool_call_with_exact_evidence",
        )
        .await?;
    let updated = mutation_doc_id(&response, "update_AgentToolCall")?;
    if updated != call.child.version.doc_id {
        anyhow::bail!("fork terminal transition updated a different tool call");
    }
    let terminal = verify_child_ref(
        executor,
        "AgentToolCall",
        &call.child.version.doc_id,
        Some(node_did),
    )
    .await?;
    let terminal_row = exact_snapshot(
        executor,
        "AgentToolCall",
        &terminal,
        "lifecycle_state result_doc_id result_composite_commit_cid result_signer_did omission_doc_id omission_composite_commit_cid omission_signer_did approval_doc_id approval_composite_commit_cid approval_signer_did",
    )
    .await?;
    if terminal_row.get("lifecycle_state").and_then(Value::as_str) != Some(terminal_phase.as_str())
    {
        anyhow::bail!("fork child did not reach its exact terminal phase");
    }
    let attached = exact_ref_from_fields(
        &terminal_row,
        &format!("{kind}_doc_id"),
        &format!("{kind}_composite_commit_cid"),
        &format!("{kind}_signer_did"),
        "fork child terminal evidence",
    )?;
    if attached != *evidence {
        anyhow::bail!("fork child terminal points to different exact evidence");
    }
    let other = if kind == "result" {
        "omission_doc_id"
    } else {
        "result_doc_id"
    };
    if terminal_row.get(other).and_then(Value::as_str).is_some() {
        anyhow::bail!("fork child terminal binds both result and omission");
    }
    if let Some(expected) = deferred_approval {
        let attached = exact_ref_from_fields(
            &terminal_row,
            "approval_doc_id",
            "approval_composite_commit_cid",
            "approval_signer_did",
            "fork child approval",
        )?;
        if attached != *expected {
            anyhow::bail!("fork child terminal points to a different approval");
        }
    }
    Ok(())
}

async fn copy_tool_results(
    executor: &(impl GraphqlExecutor + ?Sized),
    child_session_id: &str,
    child_agent_did: &str,
    child_requester_did: Option<&str>,
    child_conversation_doc_id: &str,
    node_did: &str,
    calls: &[ForkedToolCall],
    deferred_approvals: &HashMap<String, crate::SignedDocumentVersionRef>,
) -> Result<u32> {
    let mut copied = 0u32;
    for call in calls {
        let (source, source_phase, _terminal_phase) = match &call.evidence {
            ForkedToolEvidence::NonTerminal => continue,
            ForkedToolEvidence::Result {
                source,
                source_phase,
                terminal_phase,
            } => (source, *source_phase, *terminal_phase),
            ForkedToolEvidence::Omission {
                source,
                source_phase,
                terminal_phase,
                ..
            } => {
                let row = exact_snapshot(
                    executor,
                    "AgentToolOutputOmission",
                    source,
                    "tool_call_key agent_did requester_did session_id source_phase terminal_phase reason detail created_at",
                )
                .await?;
                let accepted =
                    exact_current_ref(executor, "AgentToolCall", &call.child.version.doc_id)
                        .await?;
                let child_row = exact_snapshot(
                    executor,
                    "AgentToolCall",
                    &accepted,
                    "tool_call_key lifecycle_state",
                )
                .await?;
                if child_row.get("lifecycle_state").and_then(Value::as_str)
                    != Some(source_phase.as_str())
                {
                    anyhow::bail!("fork omission child is not in its staged source phase");
                }
                let omission_key = accepted.version.composite_commit_cid.clone();
                require_child_key_absent(
                    executor,
                    "AgentToolOutputOmission",
                    "omission_key",
                    &omission_key,
                )
                .await?;
                let requester_field =
                    crate::session::requester_did_create_field(child_requester_did);
                let source_fields = fork_source_fields(source);
                let mutation = format!(
                    r#"mutation {{ create_AgentToolOutputOmission(input: {{
                        omission_key: "{}",
                        tool_call_key: "{}",
                        tool_call_doc_id: "{}",
                        tool_call_composite_commit_cid: "{}",
                        tool_call_signer_did: "{}",
                        agent_did: "{}",
                        {requester_field}
                        session_id: "{}",
                        source_phase: "{}",
                        terminal_phase: "{}",
                        reason: "{}",
                        detail: "{}",
                        created_at: "{}",
                        {source_fields}
                    }}) {{ _docID }} }}"#,
                    escape_graphql_string(&omission_key),
                    escape_graphql_string(
                        child_row
                            .get("tool_call_key")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ),
                    escape_graphql_string(&accepted.version.doc_id),
                    escape_graphql_string(&accepted.version.composite_commit_cid),
                    escape_graphql_string(&accepted.signer_did),
                    escape_graphql_string(child_agent_did),
                    escape_graphql_string(child_session_id),
                    source_phase.as_str(),
                    terminal_phase.as_str(),
                    escape_graphql_string(
                        row.get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ),
                    escape_graphql_string(
                        row.get("detail")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ),
                    escape_graphql_string(
                        row.get("created_at")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ),
                );
                let response = execute_mutation_with_retry(
                    executor,
                    &mutation,
                    "fork::copy_tool_output_omission",
                )
                .await?;
                let doc_id = mutation_doc_id(&response, "create_AgentToolOutputOmission")?;
                let child_omission =
                    verify_child_ref(executor, "AgentToolOutputOmission", &doc_id, Some(node_did))
                        .await?;
                require_sole_child_key(
                    executor,
                    "AgentToolOutputOmission",
                    "omission_key",
                    &omission_key,
                    &doc_id,
                )
                .await?;
                terminalize_forked_tool_call(
                    executor,
                    call,
                    &accepted,
                    "omission",
                    &child_omission,
                    deferred_approvals.get(&call.child.version.doc_id),
                    node_did,
                )
                .await?;
                copied += 1;
                continue;
            }
        };
        let row = exact_snapshot(
            executor,
            "AgentToolResult",
            source,
            "tool_name tool_input output_text model_output_truncated truncation_metadata created_at discarded_because_interrupted",
        )
        .await?;
        let accepted =
            exact_current_ref(executor, "AgentToolCall", &call.child.version.doc_id).await?;
        let child_row = exact_snapshot(
            executor,
            "AgentToolCall",
            &accepted,
            "tool_call_key lifecycle_state",
        )
        .await?;
        if child_row.get("lifecycle_state").and_then(Value::as_str) != Some(source_phase.as_str()) {
            anyhow::bail!("fork result child is not in its staged source phase");
        }
        let tool_name = row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_input = row.get("tool_input").and_then(|v| v.as_str()).unwrap_or("");
        let output_text = row
            .get("output_text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let truncated = row
            .get("model_output_truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let truncation_metadata = row
            .get("truncation_metadata")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let created_at = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let discarded = row
            .get("discarded_because_interrupted")
            .and_then(Value::as_bool);
        let tool_call_id = call
            .source_row
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let tool_call_key = format!("{child_session_id}:{tool_call_id}");
        let result_key = accepted.version.composite_commit_cid.clone();
        require_child_key_absent(executor, "AgentToolResult", "result_key", &result_key).await?;
        let requester_did_field = crate::session::requester_did_create_field(child_requester_did);
        let source_fields = fork_source_fields(&source);
        let mutation = format!(
            r#"mutation {{ create_AgentToolResult(input: {{
                    result_key: "{result_key_escaped}",
                    tool_call_key: "{tool_call_key_escaped}",
                    tool_call_doc_id: "{child_call_doc_id}",
                    tool_call_composite_commit_cid: "{child_call_cid}",
                    tool_call_signer_did: "{child_call_signer}",
                    agent_did: "{child_agent_did_escaped}",
                    {requester_did_field}
                    session_id: "{child_session_escaped}",
                    tool_name: "{tool_name_escaped}",
                    tool_input: "{tool_input_escaped}",
                    output_text: "{output_text_escaped}",
                    model_output_truncated: {truncated},
                    truncation_metadata: "{truncation_metadata_escaped}",
                    conversation_doc_id: "{child_conversation_doc_id}",
                    created_at: "{created_at_escaped}",
                    discarded_because_interrupted: {discarded},
                    {source_fields}
                }}) {{ _docID }} }}"#,
            result_key_escaped = escape_graphql_string(&result_key),
            tool_call_key_escaped = escape_graphql_string(&tool_call_key),
            child_call_doc_id = escape_graphql_string(&accepted.version.doc_id),
            child_call_cid = escape_graphql_string(&accepted.version.composite_commit_cid),
            child_call_signer = escape_graphql_string(&accepted.signer_did),
            child_agent_did_escaped = escape_graphql_string(child_agent_did),
            child_session_escaped = escape_graphql_string(child_session_id),
            tool_name_escaped = escape_graphql_string(tool_name),
            tool_input_escaped = escape_graphql_string(tool_input),
            output_text_escaped = escape_graphql_string(output_text),
            truncation_metadata_escaped = escape_graphql_string(truncation_metadata),
            child_conversation_doc_id = escape_graphql_string(child_conversation_doc_id),
            created_at_escaped = escape_graphql_string(created_at),
            discarded = discarded.unwrap_or(false),
        );
        let response =
            execute_mutation_with_retry(executor, &mutation, "fork::copy_tool_result").await?;
        let doc_id = mutation_doc_id(&response, "create_AgentToolResult")?;
        let child_result =
            verify_child_ref(executor, "AgentToolResult", &doc_id, Some(node_did)).await?;
        require_sole_child_key(
            executor,
            "AgentToolResult",
            "result_key",
            &result_key,
            &doc_id,
        )
        .await?;
        terminalize_forked_tool_call(
            executor,
            call,
            &accepted,
            "result",
            &child_result,
            deferred_approvals.get(&call.child.version.doc_id),
            node_did,
        )
        .await?;
        copied += 1;
    }
    Ok(copied)
}

async fn copy_tool_approvals(
    executor: &(impl GraphqlExecutor + ?Sized),
    child_session_id: &str,
    child_agent_did: &str,
    child_requester_did: Option<&str>,
    node_did: &str,
    calls: &[ForkedToolCall],
) -> Result<(u32, HashMap<String, crate::SignedDocumentVersionRef>)> {
    let mut copied = 0u32;
    let mut deferred = HashMap::new();
    for call in calls {
        let Some(source) = optional_exact_ref(
            &call.source_row,
            "approval_doc_id",
            "approval_composite_commit_cid",
            "approval_signer_did",
            "fork source AgentToolApproval",
        )?
        else {
            continue;
        };
        let row = exact_snapshot(
            executor,
            "AgentToolApproval",
            &source,
            "approval_id approval_key tool_call_id tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did request_id session_id agent_did requester_did decision approver_did reason created_at",
        )
        .await?;
        if row.get("tool_call_doc_id").and_then(Value::as_str)
            != Some(call.source.version.doc_id.as_str())
        {
            anyhow::bail!("source AgentToolApproval points to a different physical source call");
        }
        let source_parent = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(
                row.get("tool_call_doc_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                row.get("tool_call_composite_commit_cid")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            row.get("tool_call_signer_did")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        exact_snapshot(
            executor,
            "AgentToolCall",
            &source_parent,
            "tool_call_key session_id lifecycle_state",
        )
        .await
        .context("verifying exact historical source call for forked approval")?;
        if let ForkedToolEvidence::Omission {
            accepted,
            reason: crate::tool_call_lifecycle::evidence::ToolOutputOmissionReason::ApprovalDenied,
            ..
        } = &call.evidence
        {
            if &source_parent != accepted {
                anyhow::bail!(
                    "source approval-denied verdict and omission pin different held executions"
                );
            }
            if row.get("decision").and_then(Value::as_str) != Some("denied") {
                anyhow::bail!("approvalDenied omission does not pin a denied approval fact");
            }
        }

        let tool_call_id = call
            .source_row
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let tool_call_key = format!("{child_session_id}:{tool_call_id}");
        let approval_key = call.child.version.composite_commit_cid.clone();
        let approval_id = format!("approval-{}", call.child.version.composite_commit_cid);
        require_child_key_absent(executor, "AgentToolApproval", "approval_key", &approval_key)
            .await?;
        let requester_did_field = crate::session::requester_did_create_field(child_requester_did);
        let source_fields = fork_source_fields(&source);
        let mutation = format!(
            r#"mutation {{ create_AgentToolApproval(input: {{
                    approval_id: "{}",
                    approval_key: "{}",
                    tool_call_id: "{}",
                    tool_call_key: "{}",
                    tool_call_doc_id: "{}",
                    tool_call_composite_commit_cid: "{}",
                    tool_call_signer_did: "{}",
                    request_id: {},
                    session_id: "{}",
                    agent_did: "{}",
                    {requester_did_field}
                    decision: "{}",
                    approver_did: "{}",
                    reason: {},
                    created_at: "{}",
                    {source_fields}
                }}) {{ _docID }} }}"#,
            escape_graphql_string(&approval_id),
            escape_graphql_string(&approval_key),
            escape_graphql_string(tool_call_id),
            escape_graphql_string(&tool_call_key),
            escape_graphql_string(&call.child.version.doc_id),
            escape_graphql_string(&call.child.version.composite_commit_cid),
            escape_graphql_string(&call.child.signer_did),
            nullable_string_literal(row.get("request_id").and_then(Value::as_str)),
            escape_graphql_string(child_session_id),
            escape_graphql_string(child_agent_did),
            escape_graphql_string(
                row.get("decision")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ),
            escape_graphql_string(node_did),
            nullable_string_literal(row.get("reason").and_then(Value::as_str)),
            escape_graphql_string(
                row.get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ),
        );
        let response =
            execute_mutation_with_retry(executor, &mutation, "fork::copy_tool_approval").await?;
        let doc_id = mutation_doc_id(&response, "create_AgentToolApproval")?;
        let child_approval =
            verify_child_ref(executor, "AgentToolApproval", &doc_id, Some(node_did)).await?;
        require_sole_child_key(
            executor,
            "AgentToolApproval",
            "approval_key",
            &approval_key,
            &doc_id,
        )
        .await?;
        if call.evidence.defers_approval_binding() {
            deferred.insert(call.child.version.doc_id.clone(), child_approval);
        } else {
            attach_child_tool_fact(
                executor,
                &call.child.version.doc_id,
                "approval",
                &child_approval,
            )
            .await?;
        }
        copied += 1;
    }
    for call in calls {
        if call.evidence.defers_approval_binding()
            && !deferred.contains_key(&call.child.version.doc_id)
        {
            anyhow::bail!("approvalDenied fork source omitted its exact denied approval");
        }
    }
    Ok((copied, deferred))
}

async fn copy_compaction_entries(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    node_did: &str,
    behavior_id: &str,
    messages: &[ForkedMessage],
    cut_ts: &str,
) -> Result<u32> {
    let escaped_source = escape_graphql_string(source_session_id);
    let escaped_cut_ts = escape_graphql_string(cut_ts);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    created_at: {{ _lt: "{escaped_cut_ts}" }}
                }},
                order: {{ sequence: ASC }}
            ) {{ _docID compaction_key }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_compaction_entries query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "CompactionEntry");
    reject_logical_twins(&rows, "compaction_key", "fork source CompactionEntry")?;
    let mut copied = 0u32;
    let message_map = messages
        .iter()
        .map(|message| (message.source.version.doc_id.as_str(), message))
        .collect::<HashMap<_, _>>();
    let mut compaction_map: HashMap<String, (CompactionFactRef, CompactionFactRef)> =
        HashMap::new();
    for row in &rows {
        let source_doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source CompactionEntry missing _docID"))?;
        let source = exact_current_ref(executor, "CompactionEntry", source_doc_id).await?;
        let source_collection_version_id =
            exact_collection_version_id(executor, "CompactionEntry", &source).await?;
        let row = exact_snapshot(
            executor,
            "CompactionEntry",
            &source,
            "compaction_key session_id agent_did requester_did sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens source_manifest_version source_manifest_json created_at",
        )
        .await?;
        if row.get("session_id").and_then(Value::as_str) != Some(source_session_id) {
            anyhow::bail!("exact CompactionEntry source belongs to a different session");
        }
        let sequence = row
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("compaction sequence missing"))?;
        let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let files_read = row
            .get("files_read")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");
        let files_modified = row
            .get("files_modified")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");
        let messages_compacted = row
            .get("messages_compacted")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let original_tokens = row
            .get("original_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let compacted_tokens = row
            .get("compacted_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let source_manifest_version = row
            .get("source_manifest_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("source compaction manifest version missing"))?;
        let source_manifest_json = row
            .get("source_manifest_json")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("source compaction manifest missing"))?;
        let mut manifest: CompactionSourceManifest = serde_json::from_str(source_manifest_json)
            .context("decode exact source compaction manifest")?;
        manifest
            .validate(source_session_id, agent_did)
            .context("validate exact source compaction manifest")?;
        if manifest.behavior_id != behavior_id {
            anyhow::bail!(
                "cannot fork CompactionEntry across behavior change {} -> {behavior_id}",
                manifest.behavior_id
            );
        }
        manifest.session_id = child_session_id.to_string();
        for fact in &mut manifest.transcript_snapshot {
            let child = message_map.get(fact.doc_id.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "compaction source message {} is outside the exact fork cut",
                    fact.doc_id
                )
            })?;
            if child.source.version.composite_commit_cid != fact.composite_commit_cid
                || child.source.signer_did != fact.signer_did
                || child.source_collection_version_id != fact.collection_version_id
                || child.sequence != fact.sequence
            {
                anyhow::bail!("compaction source message exact ref changed during fork");
            }
            fact.doc_id = child.child.version.doc_id.clone();
            fact.composite_commit_cid = child.child.version.composite_commit_cid.clone();
            fact.collection_version_id = child.child_collection_version_id.clone();
            fact.signer_did = child.child.signer_did.clone();
        }
        for fact in &mut manifest.prior_compactions {
            let (source_ref, child) =
                compaction_map
                    .get(&fact.source.version.doc_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "prior compaction {} was not minted earlier in the child session",
                            fact.source.version.doc_id
                        )
                    })?;
            if source_ref.source.version.composite_commit_cid
                != fact.source.version.composite_commit_cid
                || source_ref.source.signer_did != fact.source.signer_did
                || source_ref.collection_version_id != fact.collection_version_id
                || child.sequence != fact.sequence
            {
                anyhow::bail!("prior compaction exact ref changed during fork");
            }
            *fact = child.clone();
        }
        manifest
            .validate(child_session_id, agent_did)
            .context("validate child compaction manifest")?;
        let child_manifest_json =
            crate::rendered_request::canonical_json_string(&serde_json::to_value(&manifest)?)?;
        let created_at = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let compaction_key = format!("{child_session_id}:{sequence}");
        require_child_key_absent(
            executor,
            "CompactionEntry",
            "compaction_key",
            &compaction_key,
        )
        .await?;
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let source_fields = fork_source_fields(&source);
        let mutation = format!(
            r#"mutation {{ create_CompactionEntry(input: {{
                    compaction_key: "{compaction_key_escaped}",
                    session_id: "{child_session_escaped}",
                    agent_did: "{agent_did_escaped}",
                    {requester_did_field}
                    sequence: {sequence},
                    summary: "{summary_escaped}",
                    files_read: "{files_read_escaped}",
                    files_modified: "{files_modified_escaped}",
                    messages_compacted: {messages_compacted},
                    original_tokens: {original_tokens},
                    compacted_tokens: {compacted_tokens},
                    source_manifest_version: {source_manifest_version},
                    source_manifest_json: "{source_manifest_json_escaped}",
                    created_at: "{created_at_escaped}",
                    {source_fields}
                }}) {{ _docID }} }}"#,
            compaction_key_escaped = escape_graphql_string(&compaction_key),
            child_session_escaped = escape_graphql_string(child_session_id),
            agent_did_escaped = escape_graphql_string(agent_did),
            summary_escaped = escape_graphql_string(summary),
            files_read_escaped = escape_graphql_string(files_read),
            files_modified_escaped = escape_graphql_string(files_modified),
            source_manifest_json_escaped = escape_graphql_string(&child_manifest_json),
            created_at_escaped = escape_graphql_string(created_at),
        );
        let response =
            execute_mutation_with_retry(executor, &mutation, "fork::copy_compaction_entry").await?;
        let doc_id = mutation_doc_id(&response, "create_CompactionEntry")?;
        let child = verify_child_ref(executor, "CompactionEntry", &doc_id, Some(node_did)).await?;
        let child_collection_version_id =
            exact_collection_version_id(executor, "CompactionEntry", &child).await?;
        require_sole_child_key(
            executor,
            "CompactionEntry",
            "compaction_key",
            &compaction_key,
            &doc_id,
        )
        .await?;
        compaction_map.insert(
            source.version.doc_id.clone(),
            (
                CompactionFactRef {
                    sequence: u32::try_from(sequence)
                        .context("source compaction sequence exceeds u32")?,
                    collection_version_id: source_collection_version_id,
                    source,
                },
                CompactionFactRef {
                    sequence: u32::try_from(sequence)
                        .context("forked compaction sequence exceeds u32")?,
                    collection_version_id: child_collection_version_id,
                    source: child,
                },
            ),
        );
        copied += 1;
    }
    Ok(copied)
}

async fn create_child_session_and_conversation(
    executor: &(impl GraphqlExecutor + ?Sized),
    child_session_id: &str,
    behavior_id: &str,
    source_session_id: &str,
    fork_at_user_turn: u32,
    parent_agent_did: &str,
    parent_agent_name: &str,
    requester_did: Option<&str>,
    expected_node_did: Option<&str>,
) -> Result<(String, String)> {
    let now = chrono::Utc::now().to_rfc3339();
    let child_session_escaped = escape_graphql_string(child_session_id);
    let behavior_id_escaped = escape_graphql_string(behavior_id);
    let forked_from_escaped = escape_graphql_string(source_session_id);
    let now_escaped = escape_graphql_string(&now);
    let agent_did_escaped = escape_graphql_string(parent_agent_did);
    let agent_name_escaped = escape_graphql_string(parent_agent_name);
    let requester_did_field = crate::session::requester_did_create_field(requester_did);

    let session_mutation = format!(
        r#"mutation {{
            create_AgentSession(input: {{
                session_id: "{child_session_escaped}",
                agent_name: "{agent_name_escaped}",
                agent_did: "{agent_did_escaped}",
                {requester_did_field}
                behavior_id: "{behavior_id_escaped}",
                started: "{now_escaped}",
                status: "active"
            }}) {{ _docID }}
        }}"#
    );
    let session =
        execute_mutation_with_retry(executor, &session_mutation, "fork::create_session").await?;
    let session_doc_id = mutation_doc_id(&session, "create_AgentSession")?;
    let session_ref =
        verify_child_ref(executor, "AgentSession", &session_doc_id, expected_node_did).await?;
    let node_did = session_ref.signer_did;

    let conv_mutation = format!(
        r#"mutation {{
            create_AgentConversation(input: {{
                session_id: "{child_session_escaped}",
                agent_name: "{agent_name_escaped}",
                agent_did: "{agent_did_escaped}",
                {requester_did_field}
                behavior_id: "{behavior_id_escaped}",
                title: "Forked conversation",
                preview_text: "",
                status: "active",
                created_at: "{now_escaped}",
                updated_at: "{now_escaped}",
                latest_request_id: "",
                forked_from_session_id: "{forked_from_escaped}",
                fork_at_user_turn: {fork_at_user_turn},
                forked_at: "{now_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    let conversation =
        execute_mutation_with_retry(executor, &conv_mutation, "fork::create_conversation").await?;
    let conversation_doc_id = mutation_doc_id(&conversation, "create_AgentConversation")?;
    verify_child_ref(
        executor,
        "AgentConversation",
        &conversation_doc_id,
        Some(&node_did),
    )
    .await?;
    Ok((conversation_doc_id, node_did))
}

async fn execute_mutation_with_retry(
    executor: &(impl GraphqlExecutor + ?Sized),
    mutation: &str,
    operation: &str,
) -> Result<GraphqlExecuteResponse> {
    let mut last_resp = None;
    let mut last_error = None;
    for attempt in 0..=DEFRA_DB_CONFLICT_MAX_RETRIES {
        if attempt > 0 {
            let backoff = defradb_conflict_retry_backoff(attempt - 1);
            tracing::warn!(
                operation = %operation,
                attempt = attempt,
                backoff_ms = backoff.as_millis() as u64,
                "retrying mutation"
            );
            tokio::time::sleep(backoff).await;
        }

        let started = std::time::Instant::now();
        let resp = executor.execute_graphql(mutation).await;
        let elapsed = started.elapsed();
        log_mutation_timing(operation, elapsed);

        match resp {
            Ok(resp) if !resp.has_errors() => return Ok(resp),
            Ok(resp) => {
                let retryable = is_defradb_transaction_conflict_text(&render_graphql_errors(&resp));
                tracing::warn!(
                    operation = %operation,
                    attempt = attempt,
                    errors = %render_graphql_errors(&resp),
                    elapsed_ms = elapsed.as_millis() as u64,
                    "mutation failed"
                );
                if retryable && attempt < DEFRA_DB_CONFLICT_MAX_RETRIES {
                    last_resp = Some(resp);
                    continue;
                }
                anyhow::bail!("{operation} failed: {}", render_graphql_errors(&resp));
            }
            Err(error) => {
                tracing::warn!(
                    operation = %operation,
                    attempt = attempt,
                    error = %error,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "mutation transport failed"
                );
                if attempt < DEFRA_DB_CONFLICT_MAX_RETRIES {
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }
        }
    }

    if let Some(resp) = last_resp {
        anyhow::bail!(
            "{operation} failed after {DEFRA_DB_CONFLICT_MAX_RETRIES} retries: {}",
            render_graphql_errors(&resp)
        );
    }
    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("{operation} failed without GraphQL response")))
}

fn graphql_rows(response: &GraphqlExecuteResponse, collection_name: &str) -> Vec<Value> {
    response
        .data
        .as_ref()
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn render_graphql_errors(response: &GraphqlExecuteResponse) -> String {
    Value::Array(response.errors.clone()).to_string()
}

fn nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn nullable_string_array_literal(value: Option<&[String]>) -> String {
    value
        .map(|values| {
            let values = values
                .iter()
                .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{values}]")
        })
        .unwrap_or_else(|| "null".to_string())
}

fn nullable_i64_literal(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn json_string_array(value: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        value
            .as_array()?
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
    )
}
