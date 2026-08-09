use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest, QueryResponse};
use identity::Did;
use serde_json::Value;

use super::rows::{dedupe_paths, CompactionEntryRow};
use super::*;

const COMPACTION_FACT_ATTEMPTS: usize = 3;

impl CompactionSourceManifest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: impl Into<String>,
        behavior_id: impl Into<String>,
        transcript_snapshot: Vec<MessageFactRef>,
        config_provenance: crate::ResolvedBehaviorConfigProvenance,
        prior_compactions: Vec<CompactionFactRef>,
        provider_view_message_count: usize,
        prior_compacted_message_count: usize,
        compactor_input_message_count: usize,
    ) -> Self {
        Self {
            manifest_version: COMPACTION_SOURCE_MANIFEST_VERSION,
            session_id: session_id.into(),
            behavior_id: behavior_id.into(),
            transcript_snapshot,
            config_provenance,
            prior_compactions,
            provider_view_message_count,
            prior_compacted_message_count,
            compactor_input_message_count,
        }
    }

    pub(crate) fn validate(
        &self,
        expected_session_id: &str,
        expected_agent_did: &str,
    ) -> Result<()> {
        if self.manifest_version != COMPACTION_SOURCE_MANIFEST_VERSION {
            anyhow::bail!(
                "unsupported CompactionEntry source manifest version {}",
                self.manifest_version
            );
        }
        if self.session_id.trim().is_empty() || self.session_id != expected_session_id {
            anyhow::bail!(
                "CompactionEntry source manifest session {:?} does not match {expected_session_id:?}",
                self.session_id
            );
        }
        self.config_provenance
            .validate_for_behavior(&self.behavior_id, expected_agent_did)
            .context("invalid CompactionEntry resolved config provenance")?;

        let mut previous_sequence = None;
        let mut doc_ids = BTreeSet::new();
        let mut cids = BTreeSet::new();
        for fact in &self.transcript_snapshot {
            require_complete_ref(
                &fact.doc_id,
                &fact.composite_commit_cid,
                &fact.signer_did,
                "AgentMessage",
            )?;
            require_collection_version_id(&fact.collection_version_id, "AgentMessage")?;
            if previous_sequence.is_some_and(|previous| fact.sequence <= previous) {
                anyhow::bail!(
                    "CompactionEntry transcript inputs are not in canonical sequence order"
                );
            }
            if !doc_ids.insert(fact.doc_id.as_str())
                || !cids.insert(fact.composite_commit_cid.as_str())
            {
                anyhow::bail!("CompactionEntry transcript inputs repeat an exact document version");
            }
            previous_sequence = Some(fact.sequence);
        }
        if self.transcript_snapshot.is_empty() {
            anyhow::bail!("CompactionEntry source manifest requires a non-empty transcript");
        }

        previous_sequence = None;
        doc_ids.clear();
        cids.clear();
        for fact in &self.prior_compactions {
            require_complete_ref(
                &fact.source.version.doc_id,
                &fact.source.version.composite_commit_cid,
                &fact.source.signer_did,
                "CompactionEntry",
            )?;
            require_collection_version_id(&fact.collection_version_id, "prior CompactionEntry")?;
            if previous_sequence.is_some_and(|previous| fact.sequence <= previous) {
                anyhow::bail!("prior CompactionEntry refs are not in canonical sequence order");
            }
            if !doc_ids.insert(fact.source.version.doc_id.as_str())
                || !cids.insert(fact.source.version.composite_commit_cid.as_str())
            {
                anyhow::bail!("prior CompactionEntry refs repeat an exact document version");
            }
            previous_sequence = Some(fact.sequence);
        }
        if self.prior_compacted_message_count > self.provider_view_message_count {
            anyhow::bail!(
                "CompactionEntry prior compacted count exceeds the exact provider-view input"
            );
        }
        let remaining = self
            .provider_view_message_count
            .saturating_sub(self.prior_compacted_message_count);
        if self.compactor_input_message_count > remaining {
            anyhow::bail!(
                "CompactionEntry compactor input count exceeds the exact post-prefix provider view"
            );
        }
        Ok(())
    }
}

fn require_complete_ref(doc_id: &str, cid: &str, signer_did: &str, label: &str) -> Result<()> {
    if doc_id.trim().is_empty() || cid.trim().is_empty() || signer_did.trim().is_empty() {
        anyhow::bail!("{label} exact source reference is incomplete");
    }
    Ok(())
}

fn require_collection_version_id(collection_version_id: &str, label: &str) -> Result<()> {
    if collection_version_id.trim().is_empty() {
        anyhow::bail!("{label} exact source reference has no collection version id");
    }
    Ok(())
}

fn compaction_identity(node: &EmbeddedNode, agent_did: &str) -> Result<Did> {
    let node_did = node.node_identity_did().ok_or_else(|| {
        anyhow::anyhow!("CompactionEntry persistence requires a DefraDB node signing identity")
    })?;
    if node_did != agent_did {
        anyhow::bail!(
            "CompactionEntry agent DID {agent_did} does not match node signing identity {node_did}"
        );
    }
    Did::new(agent_did).context("parsing CompactionEntry agent DID")
}

async fn execute(node: &EmbeddedNode, query: String, identity: Option<Did>) -> QueryResponse {
    node.execute_request_with_retry(
        QueryRequest::new(query).with_identity(identity),
        ExecuteRetryPolicy::default(),
    )
    .await
}

fn row_fields() -> &'static str {
    r#"_docID compaction_key session_id agent_did requester_did sequence summary
       files_read files_modified messages_compacted original_tokens compacted_tokens
       source_manifest_version source_manifest_json created_at fork_source_doc_id
       fork_source_composite_commit_cid fork_source_signer_did"#
}

fn signed_compaction_snapshot_matches(
    exact: &CompactionEntryRow,
    loaded: &CompactionEntryRow,
) -> bool {
    exact.doc_id == loaded.doc_id
        && exact.compaction_key == loaded.compaction_key
        && exact.session_id == loaded.session_id
        && exact.agent_did == loaded.agent_did
        && exact.requester_did == loaded.requester_did
        && exact.sequence == loaded.sequence
        && exact.summary == loaded.summary
        && exact.files_read == loaded.files_read
        && exact.files_modified == loaded.files_modified
        && exact.messages_compacted == loaded.messages_compacted
        && exact.original_tokens == loaded.original_tokens
        && exact.compacted_tokens == loaded.compacted_tokens
        && exact.source_manifest_version == loaded.source_manifest_version
        && exact.source_manifest_json == loaded.source_manifest_json
        && super::history::rfc3339_instants_equal(&exact.created_at, &loaded.created_at)
        && exact.fork_source_doc_id == loaded.fork_source_doc_id
        && exact.fork_source_composite_commit_cid == loaded.fork_source_composite_commit_cid
        && exact.fork_source_signer_did == loaded.fork_source_signer_did
}

#[cfg(test)]
mod signed_snapshot_tests {
    use super::*;

    fn row(created_at: &str) -> CompactionEntryRow {
        CompactionEntryRow {
            doc_id: "doc-1".to_string(),
            compaction_key: "session-1:1".to_string(),
            session_id: "session-1".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            requester_did: None,
            sequence: 1,
            summary: "summary".to_string(),
            files_read: "[]".to_string(),
            files_modified: "[]".to_string(),
            messages_compacted: 1,
            original_tokens: 100,
            compacted_tokens: 20,
            source_manifest_version: COMPACTION_SOURCE_MANIFEST_VERSION,
            source_manifest_json: "{}".to_string(),
            created_at: created_at.to_string(),
            fork_source_doc_id: None,
            fork_source_composite_commit_cid: None,
            fork_source_signer_did: None,
        }
    }

    #[test]
    fn exact_snapshot_accepts_equivalent_created_at_renderings() {
        let exact = row("2026-08-08T16:00:00Z");
        let loaded = row("2026-08-08T09:00:00-07:00");

        assert!(signed_compaction_snapshot_matches(&exact, &loaded));
    }

    #[test]
    fn exact_snapshot_rejects_different_created_at_instants_and_other_facts() {
        let exact = row("2026-08-08T16:00:00Z");
        let later = row("2026-08-08T16:00:01Z");
        assert!(!signed_compaction_snapshot_matches(&exact, &later));

        let mut changed_summary = exact.clone();
        changed_summary.summary = "different signed summary".to_string();
        assert!(!signed_compaction_snapshot_matches(
            &exact,
            &changed_summary
        ));
    }

    #[test]
    fn timeline_compaction_requires_a_signed_summary() {
        let row: gents_protocol::row::CompactionEntryRow =
            serde_json::from_value(serde_json::json!({
                "_docID": "doc-1",
                "compaction_key": "session-1:1",
                "session_id": "session-1",
                "agent_did": "did:key:zAgent",
                "sequence": 1
            }))
            .unwrap();

        let error = strict_timeline_compaction_row(&row)
            .unwrap_err()
            .to_string();
        assert!(error.contains("omitted summary"), "{error}");
    }

    #[test]
    fn compaction_query_envelope_requires_the_collection_field() {
        assert!(decode_compaction_rows(None).is_err());
        assert!(decode_compaction_rows(Some(&serde_json::json!({}))).is_err());

        let rows = decode_compaction_rows(Some(&serde_json::json!({
            "CompactionEntry": []
        })))
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn fork_derivation_rejects_payload_rebinding() {
        let source = row("2026-08-08T16:00:00Z");
        let mut child = source.clone();
        child.doc_id = "doc-child".to_string();
        child.compaction_key = "session-child:1".to_string();
        child.session_id = "session-child".to_string();
        assert!(compaction_payload_preserved(&source, &child));

        child.summary = "rebound summary".to_string();
        assert!(!compaction_payload_preserved(&source, &child));
    }

    #[test]
    fn transcript_lineage_rejects_wrong_session_agent_and_partial_request() {
        let mut row = CompactionMessageSourceRow {
            doc_id: "message-1".to_string(),
            message_key: "session-1:1".to_string(),
            session_id: "session-1".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            requester_did: None,
            request_id: Some("request-1".to_string()),
            request_doc_id: Some("request-doc-1".to_string()),
            sequence: 1,
            role: "user".to_string(),
            content: "content".to_string(),
            reasoning: None,
            timestamp: "2026-08-08T16:00:00Z".to_string(),
            fork_source_doc_id: None,
            fork_source_composite_commit_cid: None,
            fork_source_signer_did: None,
        };
        validate_message_lineage(&row, "session-1", "did:key:zAgent", 1).unwrap();

        row.session_id = "session-other".to_string();
        assert!(validate_message_lineage(&row, "session-1", "did:key:zAgent", 1).is_err());
        row.session_id = "session-1".to_string();
        row.request_doc_id = None;
        assert!(validate_message_lineage(&row, "session-1", "did:key:zAgent", 1).is_err());
    }
}

fn fork_source_ref(row: &CompactionEntryRow) -> Result<Option<crate::SignedDocumentVersionRef>> {
    match (
        row.fork_source_doc_id.as_deref(),
        row.fork_source_composite_commit_cid.as_deref(),
        row.fork_source_signer_did.as_deref(),
    ) {
        (None, None, None) => Ok(None),
        (Some(doc_id), Some(cid), Some(signer_did))
            if !doc_id.trim().is_empty()
                && !cid.trim().is_empty()
                && !signer_did.trim().is_empty() =>
        {
            Ok(Some(crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(doc_id, cid),
                signer_did,
            )))
        }
        _ => anyhow::bail!(
            "CompactionEntry {} has a partial or empty fork source reference",
            row.doc_id
        ),
    }
}

async fn load_rows(
    node: &EmbeddedNode,
    session_id: &str,
    identity: Option<Did>,
) -> Result<Vec<CompactionEntryRow>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ {} }}
        }}"#,
        row_fields()
    );
    let response = execute(node, query, identity).await;
    if response.has_errors() {
        anyhow::bail!(
            "loading CompactionEntry candidates for session_id={session_id}: {:?}",
            response.errors
        );
    }
    decode_compaction_rows(response.data.as_ref())
}

fn decode_compaction_rows(data: Option<&Value>) -> Result<Vec<CompactionEntryRow>> {
    let rows = data
        .and_then(|data| data.get("CompactionEntry"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("CompactionEntry query returned no collection field"))?;
    serde_json::from_value(rows).context("decoding CompactionEntry candidates")
}

fn reject_logical_twins(rows: &[CompactionEntryRow], session_id: &str) -> Result<()> {
    let mut keys = BTreeMap::<&str, Vec<&str>>::new();
    let mut sequences = BTreeMap::<u32, Vec<&str>>::new();
    for row in rows {
        keys.entry(&row.compaction_key)
            .or_default()
            .push(&row.doc_id);
        sequences.entry(row.sequence).or_default().push(&row.doc_id);
    }
    let key_conflicts = keys
        .into_iter()
        .filter(|(_, docs)| docs.len() > 1)
        .collect::<Vec<_>>();
    let sequence_conflicts = sequences
        .into_iter()
        .filter(|(_, docs)| docs.len() > 1)
        .collect::<Vec<_>>();
    if !key_conflicts.is_empty() || !sequence_conflicts.is_empty() {
        anyhow::bail!(
            "CompactionEntry logical fact conflict for session_id={session_id}: keys={key_conflicts:?} sequences={sequence_conflicts:?}"
        );
    }
    Ok(())
}

fn config_sources(
    provenance: &crate::ResolvedBehaviorConfigProvenance,
) -> Vec<&crate::ConfigFactRef> {
    let mut sources = vec![
        &provenance.principal,
        &provenance.behavior,
        &provenance.inference_backend,
        &provenance.inference_profile,
    ];
    if let Some(tool_selection) = provenance.tool_selection.as_ref() {
        sources.push(tool_selection);
    }
    sources.extend(provenance.datastore_tool_surfaces.iter());
    sources.extend(provenance.skills.iter());
    sources
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct CompactionMessageSourceRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    message_key: String,
    session_id: String,
    agent_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    sequence: u32,
    role: String,
    content: String,
    #[serde(default)]
    reasoning: Option<String>,
    timestamp: String,
    #[serde(default)]
    fork_source_doc_id: Option<String>,
    #[serde(default)]
    fork_source_composite_commit_cid: Option<String>,
    #[serde(default)]
    fork_source_signer_did: Option<String>,
}

fn exact_ref_from_parts(
    doc_id: Option<&str>,
    composite_commit_cid: Option<&str>,
    signer_did: Option<&str>,
    label: &str,
) -> Result<Option<crate::SignedDocumentVersionRef>> {
    match (doc_id, composite_commit_cid, signer_did) {
        (None, None, None) => Ok(None),
        (Some(doc_id), Some(cid), Some(signer_did))
            if !doc_id.trim().is_empty()
                && !cid.trim().is_empty()
                && !signer_did.trim().is_empty() =>
        {
            Ok(Some(crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(doc_id, cid),
                signer_did,
            )))
        }
        _ => anyhow::bail!("{label} has a partial or empty exact source reference"),
    }
}

fn message_fork_source_ref(
    row: &CompactionMessageSourceRow,
) -> Result<Option<crate::SignedDocumentVersionRef>> {
    exact_ref_from_parts(
        row.fork_source_doc_id.as_deref(),
        row.fork_source_composite_commit_cid.as_deref(),
        row.fork_source_signer_did.as_deref(),
        &format!("AgentMessage {} fork source", row.doc_id),
    )
}

fn validate_message_lineage(
    row: &CompactionMessageSourceRow,
    expected_session_id: &str,
    expected_agent_did: &str,
    expected_sequence: u32,
) -> Result<()> {
    if row.session_id != expected_session_id
        || row.agent_did != expected_agent_did
        || row.sequence != expected_sequence
        || row.message_key != format!("{expected_session_id}:{expected_sequence}")
    {
        anyhow::bail!(
            "AgentMessage {} does not bind compaction transcript sequence {} to session {} and agent {}",
            row.doc_id,
            expected_sequence,
            expected_session_id,
            expected_agent_did
        );
    }
    match (row.request_id.as_deref(), row.request_doc_id.as_deref()) {
        (None, None) => {}
        (Some(request_id), Some(request_doc_id))
            if !request_id.trim().is_empty() && !request_doc_id.trim().is_empty() => {}
        _ => anyhow::bail!(
            "AgentMessage {} has partial or empty request lineage",
            row.doc_id
        ),
    }
    Ok(())
}

fn decode_canonical_manifest(row: &CompactionEntryRow) -> Result<CompactionSourceManifest> {
    let manifest: CompactionSourceManifest = serde_json::from_str(&row.source_manifest_json)
        .with_context(|| format!("decoding CompactionEntry {} source manifest", row.doc_id))?;
    if manifest.manifest_version != row.source_manifest_version {
        anyhow::bail!(
            "CompactionEntry {} source manifest version column {} disagrees with JSON version {}",
            row.doc_id,
            row.source_manifest_version,
            manifest.manifest_version
        );
    }
    let canonical =
        crate::rendered_request::canonical_json_string(&serde_json::to_value(&manifest)?)?;
    if canonical != row.source_manifest_json {
        anyhow::bail!(
            "CompactionEntry {} source manifest is not canonical",
            row.doc_id
        );
    }
    Ok(manifest)
}

fn validate_compaction_row_shape(
    row: &CompactionEntryRow,
    source: &crate::SignedDocumentVersionRef,
) -> Result<CompactionSourceManifest> {
    if row.doc_id != source.version.doc_id
        || row.session_id.trim().is_empty()
        || row.agent_did.trim().is_empty()
        || row.sequence == 0
        || row.compaction_key != format!("{}:{}", row.session_id, row.sequence)
    {
        anyhow::bail!(
            "CompactionEntry {} has invalid physical/logical lineage",
            row.doc_id
        );
    }
    let manifest = decode_canonical_manifest(row)?;
    manifest.validate(&row.session_id, &row.agent_did)?;
    if row.messages_compacted as usize > manifest.compactor_input_message_count {
        anyhow::bail!(
            "CompactionEntry {} compacted count exceeds its exact compactor input",
            row.doc_id
        );
    }
    serde_json::from_str::<Vec<String>>(&row.files_read)
        .with_context(|| format!("decoding CompactionEntry {} files_read", row.doc_id))?;
    serde_json::from_str::<Vec<String>>(&row.files_modified)
        .with_context(|| format!("decoding CompactionEntry {} files_modified", row.doc_id))?;
    chrono::DateTime::parse_from_rfc3339(&row.created_at)
        .with_context(|| format!("parsing CompactionEntry {} created_at", row.doc_id))?;
    Ok(manifest)
}

async fn verify_exact_ref(
    node: &EmbeddedNode,
    collection: &str,
    logical_field: Option<(&str, Value)>,
    selection: &str,
    source: &crate::SignedDocumentVersionRef,
    expected_collection_version_id: Option<&str>,
    identity: Option<Did>,
    require_current: bool,
) -> Result<Value> {
    require_complete_ref(
        &source.version.doc_id,
        &source.version.composite_commit_cid,
        &source.signer_did,
        collection,
    )?;
    let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
        node,
        collection,
        &source.version,
        selection,
        identity.clone(),
    )
    .await
    .with_context(|| {
        format!(
            "loading composite-verified {collection} {} exact source {}",
            source.version.doc_id, source.version.composite_commit_cid
        )
    })?;
    if &snapshot.source != source {
        anyhow::bail!(
            "{collection} {} exact source signer {} disagrees with pinned signer {}",
            source.version.doc_id,
            snapshot.source.signer_did,
            source.signer_did
        );
    }
    if let Some(expected_collection_version_id) = expected_collection_version_id {
        require_collection_version_id(expected_collection_version_id, collection)?;
        if snapshot.collection_version_id != expected_collection_version_id {
            anyhow::bail!(
                "{collection} {} exact source schema {} disagrees with pinned schema {}",
                source.version.doc_id,
                snapshot.collection_version_id,
                expected_collection_version_id
            );
        }
    }
    if require_current {
        let current =
            crate::document_version::verified_current_signed_document_version_with_identity(
                node,
                collection,
                &source.version.doc_id,
                identity.clone(),
            )
            .await?;
        if &current != source {
            anyhow::bail!(
                "{collection} {} changed after the compaction input snapshot was loaded",
                source.version.doc_id
            );
        }
    }

    if let Some((field, expected)) = logical_field {
        if snapshot.document.get(field) != Some(&expected) {
            anyhow::bail!(
                "{collection} exact source {} does not bind logical field {field} to {expected}",
                source.version.composite_commit_cid
            );
        }
    }
    Ok(snapshot.document)
}

fn config_logical_field(collection: &str) -> Result<&'static str> {
    match collection {
        "AgentPrincipal" => Ok("agent_did"),
        "AgentBehavior" => Ok("behavior_id"),
        "InferenceBackend" => Ok("backend_id"),
        "InferenceProfile" => Ok("profile_id"),
        "ToolSelection" => Ok("selection_id"),
        "DatastoreToolSurface" => Ok("surface_id"),
        "Skill" => Ok("skill_id"),
        _ => anyhow::bail!("unsupported CompactionEntry config collection {collection}"),
    }
}

async fn verify_sole_config_candidate(
    node: &EmbeddedNode,
    fact: &crate::ConfigFactRef,
    identity: Option<Did>,
) -> Result<()> {
    let field = config_logical_field(&fact.collection)?;
    let escaped_logical_id = escape_graphql_string(&fact.logical_id);
    let response = execute(
        node,
        format!(
            r#"{{ {}(filter: {{ {field}: {{ _eq: "{escaped_logical_id}" }} }}) {{ _docID }} }}"#,
            fact.collection
        ),
        identity,
    )
    .await;
    if response.has_errors() {
        anyhow::bail!(
            "enumerating {} {} logical candidates: {:?}",
            fact.collection,
            fact.logical_id,
            response.errors
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get(&fact.collection))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("config logical candidate query returned no rows"))?;
    match rows.as_slice() {
        [row]
            if row.get("_docID").and_then(Value::as_str)
                == Some(fact.source.version.doc_id.as_str()) =>
        {
            Ok(())
        }
        rows => anyhow::bail!(
            "{} {} has {} visible logical candidates or resolves to a different _docID",
            fact.collection,
            fact.logical_id,
            rows.len()
        ),
    }
}

async fn load_current_transcript_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
    identity: Option<Did>,
) -> Result<Vec<MessageFactRef>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let response = execute(
        node,
        format!(
            r#"{{ AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID message_key agent_did sequence }} }}"#
        ),
        identity.clone(),
    )
    .await;
    if response.has_errors() {
        anyhow::bail!(
            "reloading exact compaction transcript candidates: {:?}",
            response.errors
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("exact compaction transcript query returned no rows"))?;
    let mut message_keys = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    let mut snapshot = Vec::with_capacity(rows.len());
    for row in rows {
        let doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("AgentMessage candidate has no _docID"))?;
        let message_key = row
            .get("message_key")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("AgentMessage candidate has no message_key"))?;
        let agent_did = row
            .get("agent_did")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("AgentMessage candidate has no agent_did"))?;
        let sequence = row
            .get("sequence")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow::anyhow!("AgentMessage candidate has invalid sequence"))?;
        if !message_keys.insert(message_key) || !sequences.insert(sequence) {
            anyhow::bail!(
                "compaction transcript became ambiguous before finalization: message_key={message_key} sequence={sequence}"
            );
        }
        let source =
            crate::document_version::verified_current_signed_document_version_with_identity(
                node,
                "AgentMessage",
                doc_id,
                identity.clone(),
            )
            .await?;
        let collection_version_id =
            crate::document_version::verified_collection_version_id_with_identity(
                node,
                "AgentMessage",
                &source,
                identity.clone(),
            )
            .await?;
        if source.signer_did != agent_did {
            anyhow::bail!(
                "AgentMessage {doc_id} signer {} does not match row agent {agent_did}",
                source.signer_did
            );
        }
        snapshot.push(MessageFactRef {
            sequence,
            doc_id: doc_id.to_owned(),
            composite_commit_cid: source.version.composite_commit_cid,
            collection_version_id,
            signer_did: source.signer_did,
        });
    }
    Ok(snapshot)
}

const COMPACTION_MESSAGE_SOURCE_FIELDS: &str = r#"
    message_key session_id agent_did requester_did request_id request_doc_id
    sequence role content reasoning timestamp fork_source_doc_id
    fork_source_composite_commit_cid fork_source_signer_did
"#;

async fn load_exact_message_source(
    node: &EmbeddedNode,
    source: &crate::SignedDocumentVersionRef,
    collection_version_id: &str,
    identity: Option<Did>,
    require_current: bool,
) -> Result<CompactionMessageSourceRow> {
    let document = verify_exact_ref(
        node,
        "AgentMessage",
        None,
        COMPACTION_MESSAGE_SOURCE_FIELDS,
        source,
        Some(collection_version_id),
        identity,
        require_current,
    )
    .await?;
    serde_json::from_value(document).context("decoding exact AgentMessage compaction source")
}

fn message_payload_preserved(
    source: &CompactionMessageSourceRow,
    child: &CompactionMessageSourceRow,
) -> bool {
    source.request_id == child.request_id
        && source.request_doc_id == child.request_doc_id
        && source.sequence == child.sequence
        && source.role == child.role
        && source.content == child.content
        && source.reasoning == child.reasoning
        && source.timestamp == child.timestamp
}

fn compaction_payload_preserved(source: &CompactionEntryRow, child: &CompactionEntryRow) -> bool {
    source.sequence == child.sequence
        && source.summary == child.summary
        && source.files_read == child.files_read
        && source.files_modified == child.files_modified
        && source.messages_compacted == child.messages_compacted
        && source.original_tokens == child.original_tokens
        && source.compacted_tokens == child.compacted_tokens
        && source.source_manifest_version == child.source_manifest_version
        && super::history::rfc3339_instants_equal(&source.created_at, &child.created_at)
}

struct VerifiedForkSource {
    row: CompactionEntryRow,
    manifest: CompactionSourceManifest,
    source: crate::SignedDocumentVersionRef,
}

async fn verify_forked_compaction_derivation(
    node: &EmbeddedNode,
    child_row: &CompactionEntryRow,
    child_source: &crate::SignedDocumentVersionRef,
    child_manifest: &CompactionSourceManifest,
    identity: Option<Did>,
) -> Result<VerifiedForkSource> {
    let fork_source = fork_source_ref(child_row)?.ok_or_else(|| {
        anyhow::anyhow!(
            "CompactionEntry {} has no complete fork source",
            child_row.doc_id
        )
    })?;
    if fork_source.version.doc_id == child_row.doc_id {
        anyhow::bail!(
            "CompactionEntry {} cannot derive from itself",
            child_row.doc_id
        );
    }
    let source_document = verify_exact_ref(
        node,
        "CompactionEntry",
        None,
        row_fields(),
        &fork_source,
        None,
        identity.clone(),
        false,
    )
    .await
    .context("verifying exact fork source CompactionEntry")?;
    let source_row: CompactionEntryRow = serde_json::from_value(source_document)
        .context("decoding exact fork source CompactionEntry")?;
    let source_manifest = validate_compaction_row_shape(&source_row, &fork_source)?;
    if source_row.session_id == child_row.session_id
        || source_row.agent_did != child_row.agent_did
        || !compaction_payload_preserved(&source_row, child_row)
    {
        anyhow::bail!(
            "CompactionEntry {} is not a payload-preserving cross-session derivation of {}",
            child_row.doc_id,
            source_row.doc_id
        );
    }
    if source_manifest.behavior_id != child_manifest.behavior_id
        || source_manifest.config_provenance != child_manifest.config_provenance
        || source_manifest.provider_view_message_count != child_manifest.provider_view_message_count
        || source_manifest.prior_compacted_message_count
            != child_manifest.prior_compacted_message_count
        || source_manifest.compactor_input_message_count
            != child_manifest.compactor_input_message_count
        || source_manifest.transcript_snapshot.len() != child_manifest.transcript_snapshot.len()
        || source_manifest.prior_compactions.len() != child_manifest.prior_compactions.len()
    {
        anyhow::bail!(
            "CompactionEntry {} fork manifest is not the deterministic source-manifest transform",
            child_row.doc_id
        );
    }

    for (source_fact, child_fact) in source_manifest
        .transcript_snapshot
        .iter()
        .zip(&child_manifest.transcript_snapshot)
    {
        if source_fact.sequence != child_fact.sequence {
            anyhow::bail!(
                "CompactionEntry {} fork transcript changed sequence order",
                child_row.doc_id
            );
        }
        let child_message_source = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(&child_fact.doc_id, &child_fact.composite_commit_cid),
            &child_fact.signer_did,
        );
        let child_message = load_exact_message_source(
            node,
            &child_message_source,
            &child_fact.collection_version_id,
            identity.clone(),
            false,
        )
        .await?;
        validate_message_lineage(
            &child_message,
            &child_row.session_id,
            &child_row.agent_did,
            child_fact.sequence,
        )?;
        let expected_source = crate::SignedDocumentVersionRef::new(
            crate::DocumentVersionRef::new(&source_fact.doc_id, &source_fact.composite_commit_cid),
            &source_fact.signer_did,
        );
        if message_fork_source_ref(&child_message)?.as_ref() != Some(&expected_source) {
            anyhow::bail!(
                "CompactionEntry {} fork transcript does not pin the exact source message",
                child_row.doc_id
            );
        }
        let source_message = load_exact_message_source(
            node,
            &expected_source,
            &source_fact.collection_version_id,
            identity.clone(),
            false,
        )
        .await?;
        validate_message_lineage(
            &source_message,
            &source_row.session_id,
            &source_row.agent_did,
            source_fact.sequence,
        )?;
        if !message_payload_preserved(&source_message, &child_message) {
            anyhow::bail!(
                "CompactionEntry {} fork transcript rebound a source message payload",
                child_row.doc_id
            );
        }
    }

    for (source_fact, child_fact) in source_manifest
        .prior_compactions
        .iter()
        .zip(&child_manifest.prior_compactions)
    {
        if source_fact.sequence != child_fact.sequence {
            anyhow::bail!(
                "CompactionEntry {} fork changed prior-compaction order",
                child_row.doc_id
            );
        }
        let child_document = verify_exact_ref(
            node,
            "CompactionEntry",
            None,
            row_fields(),
            &child_fact.source,
            Some(&child_fact.collection_version_id),
            identity.clone(),
            false,
        )
        .await?;
        let child_prior: CompactionEntryRow = serde_json::from_value(child_document)
            .context("decoding forked prior CompactionEntry")?;
        if fork_source_ref(&child_prior)?.as_ref() != Some(&source_fact.source) {
            anyhow::bail!(
                "CompactionEntry {} fork prior does not pin the exact source compaction",
                child_row.doc_id
            );
        }
    }

    if child_source.signer_did.trim().is_empty() {
        anyhow::bail!("forked CompactionEntry has no verified child signer");
    }
    Ok(VerifiedForkSource {
        row: source_row,
        manifest: source_manifest,
        source: fork_source,
    })
}

struct ManifestVerificationTask {
    manifest: CompactionSourceManifest,
    session_id: String,
    agent_did: String,
    ancestry: BTreeSet<(String, String)>,
}

async fn verify_manifest_sources(
    node: &EmbeddedNode,
    manifest: &CompactionSourceManifest,
    identity: Option<Did>,
    require_current: bool,
) -> Result<()> {
    let expected_agent_did = manifest.config_provenance.principal.logical_id.clone();
    let mut pending = vec![ManifestVerificationTask {
        manifest: manifest.clone(),
        session_id: manifest.session_id.clone(),
        agent_did: expected_agent_did,
        ancestry: BTreeSet::new(),
    }];
    let mut verified_nodes = 0usize;
    while let Some(task) = pending.pop() {
        verified_nodes += 1;
        if verified_nodes > 1024 {
            anyhow::bail!("CompactionEntry source graph exceeds 1024 recursive manifests");
        }
        task.manifest.validate(&task.session_id, &task.agent_did)?;
        for fact in &task.manifest.transcript_snapshot {
            let source = crate::SignedDocumentVersionRef::new(
                crate::DocumentVersionRef::new(&fact.doc_id, &fact.composite_commit_cid),
                &fact.signer_did,
            );
            let message = load_exact_message_source(
                node,
                &source,
                &fact.collection_version_id,
                identity.clone(),
                require_current,
            )
            .await?;
            validate_message_lineage(&message, &task.session_id, &task.agent_did, fact.sequence)?;
        }
        for fact in config_sources(&task.manifest.config_provenance) {
            if require_current {
                verify_sole_config_candidate(node, fact, identity.clone()).await?;
            }
            let logical_field = config_logical_field(&fact.collection)?;
            verify_exact_ref(
                node,
                &fact.collection,
                Some((logical_field, Value::String(fact.logical_id.clone()))),
                logical_field,
                &fact.source,
                Some(&fact.collection_version_id),
                identity.clone(),
                require_current,
            )
            .await?;
        }

        let mut prior_compacted_message_count = 0usize;
        for fact in &task.manifest.prior_compactions {
            let key = (
                fact.source.version.doc_id.clone(),
                fact.source.version.composite_commit_cid.clone(),
            );
            if task.ancestry.contains(&key) {
                anyhow::bail!(
                    "CompactionEntry source graph contains a cycle through {} at {}",
                    key.0,
                    key.1
                );
            }
            let document = verify_exact_ref(
                node,
                "CompactionEntry",
                Some(("sequence", Value::from(fact.sequence))),
                row_fields(),
                &fact.source,
                Some(&fact.collection_version_id),
                identity.clone(),
                require_current,
            )
            .await?;
            let prior_row: CompactionEntryRow =
                serde_json::from_value(document).context("decoding exact prior CompactionEntry")?;
            let prior_manifest = validate_compaction_row_shape(&prior_row, &fact.source)?;
            if prior_row.session_id != task.session_id
                || prior_row.agent_did != task.agent_did
                || prior_row.sequence != fact.sequence
            {
                anyhow::bail!(
                    "prior CompactionEntry {} does not bind sequence {} to session {} and agent {}",
                    prior_row.doc_id,
                    fact.sequence,
                    task.session_id,
                    task.agent_did
                );
            }
            let mut ancestry = task.ancestry.clone();
            ancestry.insert(key);
            match fork_source_ref(&prior_row)? {
                None if fact.source.signer_did != prior_row.agent_did => anyhow::bail!(
                    "ordinary prior CompactionEntry {} signer {} does not match agent {}",
                    prior_row.doc_id,
                    fact.source.signer_did,
                    prior_row.agent_did
                ),
                Some(_) => {
                    let fork_source = verify_forked_compaction_derivation(
                        node,
                        &prior_row,
                        &fact.source,
                        &prior_manifest,
                        identity.clone(),
                    )
                    .await?;
                    let source_key = (
                        fork_source.source.version.doc_id.clone(),
                        fork_source.source.version.composite_commit_cid.clone(),
                    );
                    if ancestry.contains(&source_key) {
                        anyhow::bail!(
                            "CompactionEntry fork graph contains a cycle through {} at {}",
                            source_key.0,
                            source_key.1
                        );
                    }
                    let mut source_ancestry = ancestry.clone();
                    source_ancestry.insert(source_key);
                    pending.push(ManifestVerificationTask {
                        manifest: fork_source.manifest,
                        session_id: fork_source.row.session_id,
                        agent_did: fork_source.row.agent_did,
                        ancestry: source_ancestry,
                    });
                }
                None => {}
            }
            prior_compacted_message_count = prior_compacted_message_count
                .checked_add(prior_row.messages_compacted as usize)
                .context("prior compacted message count overflow")?;
            pending.push(ManifestVerificationTask {
                manifest: prior_manifest,
                session_id: prior_row.session_id,
                agent_did: prior_row.agent_did,
                ancestry,
            });
        }
        if prior_compacted_message_count != task.manifest.prior_compacted_message_count {
            anyhow::bail!(
                "CompactionEntry source manifest prior compacted count {} disagrees with exact prior facts {}",
                task.manifest.prior_compacted_message_count,
                prior_compacted_message_count
            );
        }
    }
    Ok(())
}

async fn verify_compaction_entry_graph(
    node: &EmbeddedNode,
    row: &CompactionEntryRow,
    source: &crate::SignedDocumentVersionRef,
    identity: Option<Did>,
) -> Result<()> {
    let manifest = validate_compaction_row_shape(row, source)?;
    match fork_source_ref(row)? {
        None if source.signer_did != row.agent_did => anyhow::bail!(
            "ordinary CompactionEntry {} signer {} does not match agent {}",
            row.doc_id,
            source.signer_did,
            row.agent_did
        ),
        Some(_) => {
            let fork_source =
                verify_forked_compaction_derivation(node, row, source, &manifest, identity.clone())
                    .await?;
            verify_manifest_sources(node, &fork_source.manifest, identity.clone(), false)
                .await
                .context("verifying fork-source CompactionEntry source graph")?;
        }
        None => {}
    }
    verify_manifest_sources(node, &manifest, identity, false).await
}

fn required_timeline_string(value: Option<&str>, field: &str) -> Result<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("timeline CompactionEntry omitted {field}"))
}

fn strict_timeline_compaction_row(
    row: &gents_protocol::row::CompactionEntryRow,
) -> Result<CompactionEntryRow> {
    let nonnegative = |value: Option<i64>, field: &str| -> Result<usize> {
        value
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| anyhow::anyhow!("timeline CompactionEntry has invalid {field}"))
    };
    Ok(CompactionEntryRow {
        doc_id: required_timeline_string(row.doc_id.as_deref(), "_docID")?,
        compaction_key: row.compaction_key.clone(),
        session_id: required_timeline_string(row.session_id.as_deref(), "session_id")?,
        agent_did: required_timeline_string(row.agent_did.as_deref(), "agent_did")?,
        requester_did: row.requester_did.clone(),
        sequence: row
            .sequence
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow::anyhow!("timeline CompactionEntry has invalid sequence"))?,
        summary: required_timeline_string(row.summary.as_deref(), "summary")?,
        files_read: required_timeline_string(row.files_read.as_deref(), "files_read")?,
        files_modified: required_timeline_string(row.files_modified.as_deref(), "files_modified")?,
        messages_compacted: row
            .messages_compacted
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                anyhow::anyhow!("timeline CompactionEntry has invalid messages_compacted")
            })?,
        original_tokens: nonnegative(row.original_tokens, "original_tokens")?,
        compacted_tokens: nonnegative(row.compacted_tokens, "compacted_tokens")?,
        source_manifest_version: row
            .source_manifest_version
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                anyhow::anyhow!("timeline CompactionEntry has invalid source_manifest_version")
            })?,
        source_manifest_json: required_timeline_string(
            row.source_manifest_json.as_deref(),
            "source_manifest_json",
        )?,
        created_at: required_timeline_string(row.created_at.as_deref(), "created_at")?,
        fork_source_doc_id: row.fork_source_doc_id.clone(),
        fork_source_composite_commit_cid: row.fork_source_composite_commit_cid.clone(),
        fork_source_signer_did: row.fork_source_signer_did.clone(),
    })
}

/// Re-verify a finalized compaction row and its complete exact-source graph for
/// a timeline/projection read. This deliberately validates the outer signed
/// fact too: an embedded manifest is not self-authenticating, and a fork edge
/// cannot by itself authorize payload rebinding.
pub(crate) async fn verify_compaction_entry_for_timeline(
    node: &EmbeddedNode,
    row: &gents_protocol::row::CompactionEntryRow,
    source: &crate::SignedDocumentVersionRef,
) -> Result<()> {
    let node_did = node.node_identity_did().ok_or_else(|| {
        anyhow::anyhow!("timeline compaction verification requires a DefraDB node identity")
    })?;
    let identity = Did::new(node_did).context("parsing timeline compaction reader DID")?;
    let row = strict_timeline_compaction_row(row)?;
    verify_compaction_entry_graph(node, &row, source, Some(identity)).await
}

async fn verify_compaction_row(
    node: &EmbeddedNode,
    row: &CompactionEntryRow,
    identity: Option<Did>,
) -> Result<CompactionFactRef> {
    let source = crate::document_version::verified_current_signed_document_version_with_identity(
        node,
        "CompactionEntry",
        &row.doc_id,
        identity.clone(),
    )
    .await?;
    let collection_version_id =
        crate::document_version::verified_collection_version_id_with_identity(
            node,
            "CompactionEntry",
            &source,
            identity.clone(),
        )
        .await?;
    let escaped_cid = escape_graphql_string(&source.version.composite_commit_cid);
    let response = execute(
        node,
        format!(
            r#"{{ CompactionEntry(cid: ["{escaped_cid}"]) {{ {} }} }}"#,
            row_fields()
        ),
        identity.clone(),
    )
    .await;
    if response.has_errors() {
        anyhow::bail!(
            "loading CompactionEntry {} exact snapshot {}: {:?}",
            row.doc_id,
            source.version.composite_commit_cid,
            response.errors
        );
    }
    let exact: Vec<CompactionEntryRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    match exact.as_slice() {
        [exact] if signed_compaction_snapshot_matches(exact, row) => {}
        [exact] => anyhow::bail!(
            "CompactionEntry {} current signed snapshot does not match loaded facts: exact={exact:?}",
            row.doc_id
        ),
        rows => anyhow::bail!(
            "CompactionEntry CID {} reconstructed {} documents, expected one",
            source.version.composite_commit_cid,
            rows.len()
        ),
    }
    verify_compaction_entry_graph(node, row, &source, identity).await?;
    Ok(CompactionFactRef {
        sequence: row.sequence,
        collection_version_id,
        source,
    })
}

async fn load_compaction_entries_with_identity(
    node: &EmbeddedNode,
    session_id: &str,
    identity: Option<Did>,
) -> Result<LoadedCompactionEntries> {
    let rows = load_rows(node, session_id, identity.clone()).await?;
    reject_logical_twins(&rows, session_id)?;
    let mut entries = Vec::with_capacity(rows.len());
    let mut fact_refs = Vec::with_capacity(rows.len());
    let mut previous_sequence = None;
    for row in rows {
        if previous_sequence.is_some_and(|previous| row.sequence <= previous) {
            anyhow::bail!(
                "CompactionEntry rows for session_id={session_id} are not in canonical sequence order"
            );
        }
        let fact_ref = verify_compaction_row(node, &row, identity.clone()).await?;
        entries.push(CompactionEntry::try_from(row)?);
        fact_refs.push(fact_ref);
        previous_sequence = fact_refs.last().map(|fact| fact.sequence);
    }
    let compacted_message_count = entries
        .iter()
        .map(|entry| entry.messages_compacted as i64)
        .sum::<i64>();
    tracing::Span::current().record("compaction_entry_count", entries.len() as i64);
    tracing::Span::current().record("compacted_message_count", compacted_message_count);
    Ok(LoadedCompactionEntries { entries, fact_refs })
}

pub async fn load_compaction_entries(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Vec<CompactionEntry>> {
    let node_did = node.node_identity_did().ok_or_else(|| {
        anyhow::anyhow!("loading CompactionEntry facts requires a DefraDB node identity")
    })?;
    let identity = Did::new(node_did).context("parsing CompactionEntry query identity")?;
    Ok(
        load_compaction_entries_with_identity(node, session_id, Some(identity))
            .await?
            .entries,
    )
}

pub(crate) async fn load_compaction_entries_for_agent(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
) -> Result<LoadedCompactionEntries> {
    let identity = compaction_identity(node, agent_did)?;
    load_compaction_entries_with_identity(node, session_id, Some(identity)).await
}

#[cfg(test)]
pub(crate) async fn create_test_config_provenance(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) -> Result<crate::ResolvedBehaviorConfigProvenance> {
    let backend_id = format!("{behavior_id}-backend");
    let profile_id = format!("{behavior_id}-profile");
    let mutation = format!(
        r#"mutation {{
            principal: create_AgentPrincipal(input: {{
                agent_did: "{}"
                default_behavior_id: "{}"
                enabled: true
            }}) {{ _docID }}
            behavior: create_AgentBehavior(input: {{
                behavior_id: "{}"
                agent_did: "{}"
                backend_id: "{}"
                inference_profile_id: "{}"
                enabled: true
            }}) {{ _docID }}
            backend: create_InferenceBackend(input: {{
                backend_id: "{}"
                enabled: true
            }}) {{ _docID }}
            profile: create_InferenceProfile(input: {{
                profile_id: "{}"
            }}) {{ _docID }}
        }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(behavior_id),
        escape_graphql_string(behavior_id),
        escape_graphql_string(agent_did),
        escape_graphql_string(&backend_id),
        escape_graphql_string(&profile_id),
        escape_graphql_string(&backend_id),
        escape_graphql_string(&profile_id),
    );
    let node_did = node
        .node_identity_did()
        .ok_or_else(|| anyhow::anyhow!("test config facts require a node identity"))?;
    let identity = Did::new(node_did).context("parsing test config node identity")?;
    let response = execute(node, mutation, Some(identity)).await;
    if response.has_errors() {
        anyhow::bail!("creating exact test config facts: {:?}", response.errors);
    }
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("test config mutation returned no data"))?;
    let doc_id = |alias: &str| -> Result<String> {
        data.get(alias)
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("test config mutation returned no {alias} _docID"))
    };
    let sources = [
        ("AgentPrincipal", agent_did, doc_id("principal")?),
        ("AgentBehavior", behavior_id, doc_id("behavior")?),
        ("InferenceBackend", backend_id.as_str(), doc_id("backend")?),
        ("InferenceProfile", profile_id.as_str(), doc_id("profile")?),
    ];
    let mut facts = Vec::with_capacity(sources.len());
    for (collection, logical_id, doc_id) in sources {
        let source = crate::document_version::verified_current_signed_document_version(
            node, collection, &doc_id,
        )
        .await?;
        let collection_version_id =
            crate::document_version::verified_collection_version_id_with_identity(
                node, collection, &source, None,
            )
            .await?;
        facts.push(crate::ConfigFactRef::new(
            collection,
            logical_id,
            collection_version_id,
            source,
        ));
    }
    Ok(crate::ResolvedBehaviorConfigProvenance {
        principal: facts.remove(0),
        behavior: facts.remove(0),
        inference_backend: facts.remove(0),
        inference_profile: facts.remove(0),
        tool_selection: None,
        datastore_tool_surfaces: Vec::new(),
        skills: Vec::new(),
        resolution_algorithm_version: 1,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn save_compaction_entry(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    summary: &str,
    files_read: &[String],
    files_modified: &[String],
    messages_compacted: u32,
    original_tokens: usize,
    compacted_tokens: usize,
    source_manifest: CompactionSourceManifest,
) -> Result<CompactionEntry> {
    save_compaction_entry_with_requester_did(
        node,
        session_id,
        agent_did,
        None,
        summary,
        files_read,
        files_modified,
        messages_compacted,
        original_tokens,
        compacted_tokens,
        source_manifest,
    )
    .await
}

fn desired_matches(row: &CompactionEntryRow, desired: &CompactionEntryRow) -> bool {
    row.compaction_key == desired.compaction_key
        && row.session_id == desired.session_id
        && row.agent_did == desired.agent_did
        && row.requester_did == desired.requester_did
        && row.sequence == desired.sequence
        && row.summary == desired.summary
        && row.files_read == desired.files_read
        && row.files_modified == desired.files_modified
        && row.messages_compacted == desired.messages_compacted
        && row.original_tokens == desired.original_tokens
        && row.compacted_tokens == desired.compacted_tokens
        && row.source_manifest_version == desired.source_manifest_version
        && row.source_manifest_json == desired.source_manifest_json
        && super::history::rfc3339_instants_equal(&row.created_at, &desired.created_at)
}

fn candidate_for_desired(
    rows: Vec<CompactionEntryRow>,
    desired: &CompactionEntryRow,
) -> Result<Option<CompactionEntryRow>> {
    let candidates = rows
        .into_iter()
        .filter(|row| {
            row.compaction_key == desired.compaction_key || row.sequence == desired.sequence
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [] => Ok(None),
        [row] if desired_matches(row, desired) => Ok(Some(row.clone())),
        [row] => anyhow::bail!(
            "CompactionEntry finalized fact conflict: _docID={} key={} sequence={}",
            row.doc_id,
            row.compaction_key,
            row.sequence
        ),
        rows => anyhow::bail!(
            "CompactionEntry logical fact conflict for key={} sequence={}: _docIDs={:?}",
            desired.compaction_key,
            desired.sequence,
            rows.iter()
                .map(|row| row.doc_id.as_str())
                .collect::<Vec<_>>()
        ),
    }
}

fn mutation_doc_ids(data: Option<&Value>) -> Vec<String> {
    data.and_then(Value::as_object)
        .into_iter()
        .flat_map(|object| object.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|row| row.get("_docID").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_compaction_entry_with_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    summary: &str,
    files_read: &[String],
    files_modified: &[String],
    messages_compacted: u32,
    original_tokens: usize,
    compacted_tokens: usize,
    source_manifest: CompactionSourceManifest,
) -> Result<CompactionEntry> {
    let identity = compaction_identity(node, agent_did)?;
    source_manifest.validate(session_id, agent_did)?;
    if messages_compacted as usize > source_manifest.compactor_input_message_count {
        anyhow::bail!("CompactionEntry compacted count exceeds its exact compactor input");
    }
    verify_manifest_sources(node, &source_manifest, Some(identity.clone()), true)
        .await
        .context("re-verifying exact CompactionEntry inputs before finalization")?;
    let current_transcript =
        load_current_transcript_snapshot(node, session_id, Some(identity.clone())).await?;
    if current_transcript != source_manifest.transcript_snapshot {
        anyhow::bail!(
            "CompactionEntry transcript snapshot changed or became ambiguous before finalization"
        );
    }

    let previous =
        load_compaction_entries_with_identity(node, session_id, Some(identity.clone())).await?;
    if previous.fact_refs != source_manifest.prior_compactions {
        anyhow::bail!("CompactionEntry prior exact snapshot changed before finalization");
    }
    let observed_prior_count = previous
        .entries
        .iter()
        .map(|entry| entry.messages_compacted as usize)
        .sum::<usize>();
    if observed_prior_count != source_manifest.prior_compacted_message_count {
        anyhow::bail!("CompactionEntry prior compacted count disagrees with exact prior facts");
    }

    let mut cumulative_files_read = previous
        .entries
        .last()
        .map(|entry| entry.files_read.clone())
        .unwrap_or_default();
    cumulative_files_read.extend(files_read.iter().cloned());
    dedupe_paths(&mut cumulative_files_read);
    let mut cumulative_files_modified = previous
        .entries
        .last()
        .map(|entry| entry.files_modified.clone())
        .unwrap_or_default();
    cumulative_files_modified.extend(files_modified.iter().cloned());
    dedupe_paths(&mut cumulative_files_modified);

    let sequence = previous
        .fact_refs
        .last()
        .map_or(1, |entry| entry.sequence + 1);
    let compaction_key = format!("{session_id}:{sequence}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let source_manifest_json =
        crate::rendered_request::canonical_json_string(&serde_json::to_value(&source_manifest)?)?;
    let desired = CompactionEntryRow {
        doc_id: String::new(),
        compaction_key: compaction_key.clone(),
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        requester_did: requester_did.map(str::to_owned),
        sequence,
        summary: summary.trim().to_string(),
        files_read: serde_json::to_string(&cumulative_files_read)?,
        files_modified: serde_json::to_string(&cumulative_files_modified)?,
        messages_compacted,
        original_tokens,
        compacted_tokens,
        source_manifest_version: COMPACTION_SOURCE_MANIFEST_VERSION,
        source_manifest_json,
        created_at,
        fork_source_doc_id: None,
        fork_source_composite_commit_cid: None,
        fork_source_signer_did: None,
    };

    for attempt in 1..=COMPACTION_FACT_ATTEMPTS {
        let rows = load_rows(node, session_id, Some(identity.clone())).await?;
        reject_logical_twins(&rows, session_id)?;
        if let Some(existing) = candidate_for_desired(rows, &desired)? {
            verify_compaction_row(node, &existing, Some(identity.clone())).await?;
            return CompactionEntry::try_from(existing);
        }

        let requester_did_field = super::requester_did_create_field(requester_did);
        let mutation = format!(
            r#"mutation {{
                create_CompactionEntry(input: {{
                    compaction_key: "{compaction_key}",
                    session_id: "{session_id}",
                    agent_did: "{agent_did}",
                    {requester_did_field}
                    sequence: {sequence},
                    summary: "{summary}",
                    files_read: "{files_read}",
                    files_modified: "{files_modified}",
                    messages_compacted: {messages_compacted},
                    original_tokens: {original_tokens},
                    compacted_tokens: {compacted_tokens},
                    source_manifest_version: {source_manifest_version},
                    source_manifest_json: "{source_manifest_json}",
                    created_at: "{created_at}"
                }}) {{ _docID }}
            }}"#,
            compaction_key = escape_graphql_string(&desired.compaction_key),
            session_id = escape_graphql_string(&desired.session_id),
            agent_did = escape_graphql_string(&desired.agent_did),
            summary = escape_graphql_string(&desired.summary),
            files_read = escape_graphql_string(&desired.files_read),
            files_modified = escape_graphql_string(&desired.files_modified),
            messages_compacted = desired.messages_compacted,
            original_tokens = desired.original_tokens,
            compacted_tokens = desired.compacted_tokens,
            source_manifest_version = desired.source_manifest_version,
            source_manifest_json = escape_graphql_string(&desired.source_manifest_json),
            created_at = escape_graphql_string(&desired.created_at),
        );
        let response = execute(node, mutation, Some(identity.clone())).await;
        if !response.has_errors() {
            let returned = mutation_doc_ids(response.data.as_ref());
            if returned.len() != 1 {
                anyhow::bail!("creating CompactionEntry returned unexpected _docIDs={returned:?}");
            }
            let rows = load_rows(node, session_id, Some(identity.clone())).await?;
            reject_logical_twins(&rows, session_id)?;
            let persisted = candidate_for_desired(rows, &desired)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "created CompactionEntry {} was not observable by exact logical key/order",
                    returned[0]
                )
            })?;
            if persisted.doc_id != returned[0] {
                anyhow::bail!(
                    "created CompactionEntry returned _docID={} but observed {}",
                    returned[0],
                    persisted.doc_id
                );
            }
            verify_compaction_row(node, &persisted, Some(identity.clone())).await?;
            return CompactionEntry::try_from(persisted);
        }
        if attempt == COMPACTION_FACT_ATTEMPTS {
            anyhow::bail!("creating CompactionEntry failed: {:?}", response.errors);
        }
        tokio::task::yield_now().await;
    }
    unreachable!("bounded CompactionEntry persistence loop returns")
}
