//! The durable fact record for one provider call (#840): DefraDB-backed
//! pieces only.
//!
//! The capture mechanism itself (the arm/claim scope, the capturing
//! transport, the pure DTO builder) moved to `gents-loop` (G-1); this module
//! re-exports it so `crate::rendered_request` keeps every symbol this crate's
//! callers already use, and keeps the two pieces that cannot move: the
//! DefraDB-backed sink (`sink`, `commits`) and the admission-controller
//! provenance lookup, since a guest has neither a database nor an admission
//! controller.

pub mod commits;
pub(crate) mod encoding;
pub mod sink;

pub use gents_loop::rendered_request::{
    build_rendered_completion_request, canonical_json, canonical_json_string, capture_key,
    sha256_canonical_json,
};
pub use gents_loop::rendered_request::{
    scope, transport, AdmissionJoin, AssemblyBuildPath, AssemblyTrace, AssistantMessageId,
    CaptureOrderKey, CaptureScope, CaptureScopeKind, CaptureSeam, ContextAccounting,
    ContextCompactionReason, ContextInputComponents, ParsedProvenance, ProvenanceManifest,
    ProvenanceStatus, RenderedCompletionRequest, RenderedRequestCaptureFactory,
    RenderedRequestCaptureSink, RenderedRequestCapturingHttpClient, RenderedRequestComponents,
    RenderedRequestContext, RenderedRequestSource, ThreadedToolResult, ASSEMBLY_TRACE_VERSION,
    CAPTURE_VERSION, CONTEXT_ACCOUNTING_VERSION, PROVENANCE_MANIFEST_VERSION,
};
pub(crate) use sink::defra_rendered_request_capture_factory;
pub use sink::DefraRenderedRequestSink;

/// The admission identity to stamp into this capture's provenance, if the call
/// in flight on this task belongs to the loop the capture describes.
///
/// The kind guard keeps the task-local admission join honest if a caller wires
/// the wrong capture scope. One-shot captures never join because no admission
/// scope exists there. Installed onto every production `RequestCaptureScope`
/// via `scope_from_factory` below; the loop's own default is `|_| None`.
fn admission_join_for_scope(capture_scope: &str) -> Option<AdmissionJoin> {
    let join = crate::admission::current_call_join()?;
    let scope_kind = capture_scope.parse::<CaptureScope>().ok()?.kind;
    admission_kind_matches_scope(join.call_kind, scope_kind).then(|| AdmissionJoin {
        call_id: join.call_id,
        call_seq: join.call_seq,
    })
}

/// Which admission [`CallKind`](crate::admission::CallKind) legitimately
/// produces captures of which [`CaptureScopeKind`]. `OneShot` maps to nothing:
/// one-shot runs have no admission scope at all, so a join observed under a
/// oneshot capture could only be another loop's call.
pub(crate) fn admission_kind_matches_scope(
    call_kind: crate::admission::CallKind,
    scope_kind: CaptureScopeKind,
) -> bool {
    use crate::admission::CallKind;

    matches!(
        (call_kind, scope_kind),
        (CallKind::Inference, CaptureScopeKind::Inference)
            | (CallKind::Compaction, CaptureScopeKind::Compaction)
            | (CallKind::Compaction, CaptureScopeKind::CompactionFallback)
            | (CallKind::OneOff, CaptureScopeKind::Title)
    )
}

/// `RenderedRequestContext` for a claimed durable request. The loop-side
/// struct dropped this constructor (it names the native `AgentRequest`
/// document type); this is its native replacement.
pub(crate) fn context_for_claimed_request(
    request: &crate::watcher::AgentRequest,
    request_commit_cid: &str,
    model_name: String,
    provider_family: Option<String>,
) -> RenderedRequestContext {
    RenderedRequestContext {
        request_doc_id: request.doc_id.clone(),
        request_commit_cid: request_commit_cid.to_string(),
        request_id: request.request_id.clone(),
        agent_did: request.agent_did.clone(),
        requester_did: request.requester_did.clone().unwrap_or_default(),
        behavior_id: request.behavior_id.clone(),
        session_id: request.session_id.clone(),
        model_name,
        provider_family,
    }
}

/// Build a capture scope from a context and an optional factory, with the
/// native admission-join lookup installed. `None` when capture is not
/// configured.
pub(crate) fn scope_from_factory(
    context: RenderedRequestContext,
    factory: Option<&RenderedRequestCaptureFactory>,
) -> Option<std::sync::Arc<scope::RequestCaptureScope>> {
    let factory = factory?;
    let sink = factory(context.clone());
    Some(std::sync::Arc::new(
        scope::RequestCaptureScope::new(context, sink)
            .with_admission_join_lookup(std::sync::Arc::new(admission_join_for_scope)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A delta capture whose base must be read from the store.
    fn delta_capture() -> String {
        let mut value = serde_json::json!({"model": "m", "stream": true});
        value["padding"] = serde_json::json!("x".repeat(4096));
        let mut base_value = value.clone();
        base_value["stream"] = serde_json::json!(false);
        let request = encoding::encode_against(
            &value,
            &base_value,
            encoding::BaseWitness {
                doc_id: "bae-base".into(),
                field_commit_cid: "base-commit".into(),
                depth: 0,
                agent_did: "did:test".into(),
                requester_did: String::new(),
                session_id: "session".into(),
                source: "openai_responses".into(),
                capture_scope: "inference.1".into(),
            },
        )
        .unwrap();
        let provenance = encoding::encode_full(&serde_json::json!({})).unwrap();
        let stored = encoding::encode_container(&request, &provenance).unwrap();
        assert!(stored.contains("base-commit"), "fixture must store a delta");
        stored
    }

    struct FailingStore;

    #[async_trait::async_trait]
    impl CaptureBaseReader for FailingStore {
        async fn execute_capture_query(&self, _query: &str) -> Result<Value> {
            anyhow::bail!("store unavailable")
        }
        async fn capture_field_commit(
            &self,
            _doc_id: &str,
            _field: &str,
        ) -> Result<Option<commits::RequestJsonCommit>> {
            anyhow::bail!("store unavailable")
        }
    }

    struct MissingBase;

    #[async_trait::async_trait]
    impl CaptureBaseReader for MissingBase {
        async fn execute_capture_query(&self, _query: &str) -> Result<Value> {
            Ok(serde_json::json!({"data": {"RenderedRequest": []}}))
        }
        async fn capture_field_commit(
            &self,
            _doc_id: &str,
            _field: &str,
        ) -> Result<Option<commits::RequestJsonCommit>> {
            Ok(None)
        }
    }

    /// Replay drops a turn only for a capture that fails verification; a store
    /// read failure while resolving its base is typed so it propagates.
    #[tokio::test]
    async fn base_resolution_distinguishes_store_reads_from_missing_bases() {
        let stored = delta_capture();
        let store = decode_capture_json_from(
            &FailingStore,
            gents_protocol::rendered_request::CAPTURE_VERSION,
            &stored,
            CapturePayloadKind::RequestBody,
        )
        .await
        .expect_err("a failed store read cannot decode");
        assert!(
            store.downcast_ref::<CaptureStoreReadError>().is_some(),
            "{store:#}"
        );

        let missing = decode_capture_json_from(
            &MissingBase,
            gents_protocol::rendered_request::CAPTURE_VERSION,
            &stored,
            CapturePayloadKind::RequestBody,
        )
        .await
        .expect_err("a missing base cannot decode");
        assert!(
            missing.downcast_ref::<CaptureStoreReadError>().is_none(),
            "{missing:#}"
        );
    }

    fn agent_request() -> crate::watcher::AgentRequest {
        crate::watcher::AgentRequest {
            purpose: gents_protocol::request_admission::RequestPurpose::Normal,
            doc_id: "doc-1".to_string(),
            request_id: "request-1".to_string(),
            agent_did: "did:key:test".to_string(),
            requester_did: None,
            behavior_id: "behavior".to_string(),
            session_id: "session".to_string(),
            content: "hi".to_string(),
            max_total_tokens: None,
            input: Default::default(),
            execution_origin: None,
            created_at: String::new(),
            deadline: None,
            execution_generation: None,
            execution_lease_expires_at: None,
            execution_lease_secs: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_source_doc_id: None,
            caused_by_correlation: None,
            caused_by_trigger_context: None,
            workspace_id: None,
            workspace_authority: None,
            workspace_owner_agent_did: None,
            workspace_seal_hash: None,
        }
    }

    #[test]
    fn context_for_request_carries_an_absent_requester_as_empty() {
        let mut request = agent_request();
        request.requester_did = None;
        let context = context_for_claimed_request(&request, "", "test-model".to_string(), None);
        assert_eq!(context.requester_did, "");

        request.requester_did = Some("did:key:requester".to_string());
        let context = context_for_claimed_request(&request, "", "test-model".to_string(), None);
        assert_eq!(context.requester_did, "did:key:requester");
    }

    /// The kind guard: a join is stamped only when the admitted call's kind
    /// legitimately produces the capture's loop. A wrong join would be worse
    /// than none.
    #[test]
    fn admission_kinds_map_to_their_capture_scopes() {
        use crate::admission::CallKind;

        let cases = [
            (CallKind::Inference, CaptureScopeKind::Inference, true),
            (CallKind::Inference, CaptureScopeKind::Compaction, false),
            (CallKind::Compaction, CaptureScopeKind::Compaction, true),
            (
                CallKind::Compaction,
                CaptureScopeKind::CompactionFallback,
                true,
            ),
            (CallKind::Compaction, CaptureScopeKind::Inference, false),
            (CallKind::OneOff, CaptureScopeKind::Title, true),
            (CallKind::OneOff, CaptureScopeKind::OneShot, false),
            (CallKind::Inference, CaptureScopeKind::OneShot, false),
            (CallKind::Scheduled, CaptureScopeKind::Inference, false),
        ];
        for (call_kind, scope_kind, expected) in cases {
            assert_eq!(
                admission_kind_matches_scope(call_kind, scope_kind),
                expected,
                "{call_kind:?} vs {scope_kind:?}"
            );
        }
    }
}

use anyhow::{Context, Result};
pub use encoding::CapturePayloadKind;
use serde_json::Value;

pub fn decode_inline_capture_json(capture_version: u32, stored: &str) -> Result<Value> {
    encoding::resolve_capture_with(
        capture_version,
        stored,
        CapturePayloadKind::RequestBody,
        |_| anyhow::bail!("capture delta requires base resolution"),
    )
}

/// A store read that failed while resolving a capture's delta chain. It says
/// nothing about the capture itself, so replay must not treat it as an
/// unverifiable capture.
#[derive(Debug, thiserror::Error)]
#[error("reading a rendered-request capture base from the store: {0:#}")]
pub struct CaptureStoreReadError(pub anyhow::Error);

#[async_trait::async_trait]
trait CaptureBaseReader {
    async fn execute_capture_query(&self, query: &str) -> Result<Value>;
    async fn capture_field_commit(
        &self,
        doc_id: &str,
        field: &str,
    ) -> Result<Option<commits::RequestJsonCommit>>;
}

#[async_trait::async_trait]
impl CaptureBaseReader for crate::config_client::ConfigAccess {
    async fn execute_capture_query(&self, query: &str) -> Result<Value> {
        self.execute(query).await
    }

    async fn capture_field_commit(
        &self,
        doc_id: &str,
        field: &str,
    ) -> Result<Option<commits::RequestJsonCommit>> {
        commits::field_commit(self, doc_id, field).await
    }
}

#[async_trait::async_trait]
impl CaptureBaseReader for defra_node::EmbeddedNode {
    async fn execute_capture_query(&self, query: &str) -> Result<Value> {
        let response = self.execute(query).await;
        crate::graphql::ensure_no_errors(&response, "reading rendered-request delta base")?;
        Ok(serde_json::json!({"data": response.data}))
    }

    async fn capture_field_commit(
        &self,
        doc_id: &str,
        field: &str,
    ) -> Result<Option<commits::RequestJsonCommit>> {
        commits::field_commit_embedded(self, doc_id, field).await
    }
}

type CaptureBaseCache = std::collections::BTreeMap<String, (Value, String)>;

async fn decode_capture_json_from_cached<R: CaptureBaseReader + Sync>(
    reader: &R,
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
    cache: &mut CaptureBaseCache,
) -> Result<Value> {
    let mut next_version = capture_version;
    let mut next_stored = stored.to_owned();
    let mut bases = std::collections::BTreeMap::new();
    for _ in 0..=encoding::MAX_DELTA_DEPTH {
        match encoding::decode_capture_record(next_version, &next_stored, kind)? {
            encoding::DecodedRecord::Legacy(_) | encoding::DecodedRecord::Full(_) => break,
            encoding::DecodedRecord::Delta { base, .. } => {
                if !cache.contains_key(&base.doc_id) {
                    let query = format!(
                        r#"{{ RenderedRequest(filter: {{_docID: {{_eq: "{doc_id}"}}}}, limit: 2) {{
                            capture_version agent_did requester_did session_id source capture_scope request_json
                        }} }}"#,
                        doc_id = crate::graphql::escape_graphql_string(&base.doc_id),
                    );
                    let response = reader
                        .execute_capture_query(&query)
                        .await
                        .map_err(|error| anyhow::Error::new(CaptureStoreReadError(error)))?;
                    let rows = response
                        .get("data")
                        .and_then(|data| data.get("RenderedRequest"))
                        .and_then(Value::as_array)
                        .context(
                            "reading witnessed rendered-request base returned an unexpected shape",
                        )?;
                    let [row] = rows.as_slice() else {
                        anyhow::bail!("rendered-request delta base did not resolve uniquely");
                    };
                    let actual = reader
                        .capture_field_commit(&base.doc_id, "request_json")
                        .await
                        .map_err(|error| anyhow::Error::new(CaptureStoreReadError(error)))?
                        .context("rendered-request delta base lacks field commit")?
                        .cid;
                    cache.insert(base.doc_id.clone(), (row.clone(), actual));
                }
                let (row, actual) = cache
                    .get(&base.doc_id)
                    .context("rendered-request delta base cache was not populated")?;
                for (name, expected) in [
                    ("agent_did", base.agent_did.as_str()),
                    ("requester_did", base.requester_did.as_str()),
                    ("session_id", base.session_id.as_str()),
                    ("source", base.source.as_str()),
                    ("capture_scope", base.capture_scope.as_str()),
                ] {
                    anyhow::ensure!(
                        row.get(name).and_then(Value::as_str) == Some(expected),
                        "rendered-request delta base changed {name} scope"
                    );
                }
                let version = row
                    .get("capture_version")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .context("rendered-request delta base lacks capture_version")?;
                let encoded = row
                    .get("request_json")
                    .and_then(Value::as_str)
                    .context("rendered-request delta base lacks encoded field")?
                    .to_owned();
                anyhow::ensure!(
                    encoding::capture_record_depth(version, &encoded, kind)? == base.depth,
                    "rendered-request delta base depth witness is inconsistent"
                );
                bases.insert(
                    (base.doc_id.clone(), base.field_commit_cid.clone()),
                    (version, encoded.clone(), actual.clone()),
                );
                next_version = version;
                next_stored = encoded;
            }
        }
    }
    encoding::resolve_capture_with(capture_version, stored, kind, |base| {
        bases
            .remove(&(base.doc_id.clone(), base.field_commit_cid.clone()))
            .context("capture delta chain exceeds maximum depth or contains a cycle")
    })
}

async fn decode_capture_json_from<R: CaptureBaseReader + Sync>(
    reader: &R,
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
) -> Result<Value> {
    decode_capture_json_from_cached(
        reader,
        capture_version,
        stored,
        kind,
        &mut CaptureBaseCache::new(),
    )
    .await
}

async fn decode_capture_pair_selected_from<R: CaptureBaseReader + Sync>(
    reader: &R,
    capture_version: u32,
    stored: &str,
    want_request: bool,
    want_provenance: bool,
) -> Result<(Option<Value>, Option<Value>)> {
    let mut cache = CaptureBaseCache::new();
    let request = if want_request {
        Some(
            decode_capture_json_from_cached(
                reader,
                capture_version,
                stored,
                CapturePayloadKind::RequestBody,
                &mut cache,
            )
            .await?,
        )
    } else {
        None
    };
    let provenance = if want_provenance {
        Some(
            decode_capture_json_from_cached(
                reader,
                capture_version,
                stored,
                CapturePayloadKind::ProvenancePayload,
                &mut cache,
            )
            .await?,
        )
    } else {
        None
    };
    Ok((request, provenance))
}

async fn decode_capture_pair_from<R: CaptureBaseReader + Sync>(
    reader: &R,
    capture_version: u32,
    stored: &str,
) -> Result<(Value, Value)> {
    let (request, provenance) =
        decode_capture_pair_selected_from(reader, capture_version, stored, true, true).await?;
    Ok((
        request.context("capture pair omitted request body")?,
        provenance.context("capture pair omitted provenance payload")?,
    ))
}

pub(crate) async fn decode_capture_pair_selected(
    access: &crate::config_client::ConfigAccess,
    capture_version: u32,
    stored: &str,
    request: bool,
    provenance: bool,
) -> Result<(Option<Value>, Option<Value>)> {
    decode_capture_pair_selected_from(access, capture_version, stored, request, provenance).await
}

/// Decode both payloads from one capture while reusing each witnessed base.
pub async fn decode_capture_pair(
    access: &crate::config_client::ConfigAccess,
    capture_version: u32,
    stored: &str,
) -> Result<(Value, Value)> {
    decode_capture_pair_from(access, capture_version, stored).await
}

pub async fn decode_capture_json(
    access: &crate::config_client::ConfigAccess,
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
) -> Result<Value> {
    decode_capture_json_from(access, capture_version, stored, kind).await
}

/// Decode one capture payload through the same witnessed resolver used by the
/// sink, for callers that already own the embedded DefraDB node.
pub async fn decode_capture_json_embedded(
    node: &defra_node::EmbeddedNode,
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
) -> Result<Value> {
    decode_capture_json_from(node, capture_version, stored, kind).await
}
