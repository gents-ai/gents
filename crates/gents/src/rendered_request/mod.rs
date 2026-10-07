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

    /// A v2 delta capture whose base must be read from the store. Deltas are no
    /// longer written (#2333) but rows stored as deltas must keep decoding, so
    /// this fixture pins that decode against version 2 explicitly.
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
    }

    struct MissingBase;

    struct CountingBase {
        reads: std::sync::atomic::AtomicUsize,
        cid: std::sync::Mutex<String>,
    }

    #[async_trait::async_trait]
    impl CaptureBaseReader for CountingBase {
        async fn execute_capture_query(&self, _query: &str) -> Result<Value> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let value =
                serde_json::json!({"model":"m", "stream":false, "padding":"x".repeat(4096)});
            let encoded = encoding::encode_container(
                &encoding::encode_full(&value)?,
                &encoding::encode_full(&serde_json::json!({}))?,
            )?;
            Ok(serde_json::json!({"data":{"RenderedRequest":[{
                "capture_version":2, "agent_did":"did:test", "requester_did":"",
                "session_id":"session", "source":"openai_responses", "capture_scope":"inference.1",
                "request_json":encoded
            }], "_commits":[{"cid":self.cid.lock().unwrap().clone(), "height":1, "fieldName":"request_json"}]}}))
        }
    }

    #[tokio::test]
    async fn replay_batch_reuses_witnessed_bases_without_skipping_witness_checks() {
        let reader = CountingBase {
            reads: std::sync::atomic::AtomicUsize::new(0),
            cid: std::sync::Mutex::new("base-commit".into()),
        };
        let stored = delta_capture();
        let mut cache = CaptureReadCache::default();
        for _ in 0..32 {
            let decoded = decode_capture_json_from_cached(
                &reader,
                2,
                &stored,
                CapturePayloadKind::RequestBody,
                &mut cache,
            )
            .await
            .unwrap();
            assert_eq!(
                decoded,
                serde_json::json!({"model":"m", "stream":true, "padding":"x".repeat(4096)})
            );
        }
        assert_eq!(reader.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
        for changed in [
            stored.replace("base-commit", "foreign-commit"),
            stored.replace("session", "foreign-session"),
        ] {
            assert!(decode_capture_json_from_cached(
                &reader,
                CAPTURE_VERSION,
                &changed,
                CapturePayloadKind::RequestBody,
                &mut cache
            )
            .await
            .is_err());
        }
        *reader.cid.lock().unwrap() = "edited-base-commit".into();
        assert!(
            decode_capture_json_from(&reader, 2, &stored, CapturePayloadKind::RequestBody)
                .await
                .is_err()
        );
        assert_eq!(reader.reads.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[async_trait::async_trait]
    impl CaptureBaseReader for MissingBase {
        async fn execute_capture_query(&self, _query: &str) -> Result<Value> {
            Ok(serde_json::json!({"data": {"RenderedRequest": []}}))
        }
    }

    #[tokio::test]
    async fn batched_base_read_decodes_through_local_and_embedded_access() {
        use crate::config_client::ConfigAccess;
        use crate::graphql::escape_graphql_string;
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        let value = serde_json::json!({"model":"m", "stream":false, "padding":"x".repeat(4096)});
        let container = encoding::encode_container(
            &encoding::encode_full(&value).unwrap(),
            &encoding::encode_full(&serde_json::json!({})).unwrap(),
        )
        .unwrap();
        let mutation = format!(
            r#"mutation {{ create_RenderedRequest(input: {{
                capture_key: "batched-base", capture_version: 2,
                agent_did: "did:test", requester_did: "", session_id: "session",
                source: "openai_responses", capture_scope: "inference.1",
                request_json: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&container),
        );
        let created = ConfigAccess::write_local_response(&node, "test.capture_base", &mutation)
            .await
            .unwrap();
        let data = created.data.unwrap();
        let doc_id = data.as_object().unwrap().values().next().unwrap()[0]["_docID"]
            .as_str()
            .unwrap();
        let commit = commits::request_json_commit(&access, doc_id)
            .await
            .unwrap()
            .unwrap();
        let stored = delta_capture()
            .replace("bae-base", doc_id)
            .replace("base-commit", &commit.cid);
        let mut expected = value;
        expected["stream"] = serde_json::json!(true);
        assert_eq!(
            decode_capture_json(&access, 2, &stored, CapturePayloadKind::RequestBody)
                .await
                .unwrap(),
            expected
        );
        assert_eq!(
            decode_capture_json_embedded(&node, 2, &stored, CapturePayloadKind::RequestBody)
                .await
                .unwrap(),
            expected
        );
        node.shutdown().await;
    }

    /// Replay drops a turn only for a capture that fails verification; a store
    /// read failure while resolving its base is typed so it propagates.
    #[tokio::test]
    async fn base_resolution_distinguishes_store_reads_from_missing_bases() {
        let stored = delta_capture();
        let store =
            decode_capture_json_from(&FailingStore, 2, &stored, CapturePayloadKind::RequestBody)
                .await
                .expect_err("a failed store read cannot decode");
        assert!(
            store.downcast_ref::<CaptureStoreReadError>().is_some(),
            "{store:#}"
        );

        let missing =
            decode_capture_json_from(&MissingBase, 2, &stored, CapturePayloadKind::RequestBody)
                .await
                .expect_err("a missing base cannot decode");
        assert!(
            missing.downcast_ref::<CaptureStoreReadError>().is_none(),
            "{missing:#}"
        );
    }

    /// Deterministic pseudo-text: a fixed-seed xorshift64 stream mapped onto
    /// `a..z`. Uniform text almost never hits a content-defined boundary, so a
    /// body meant to span several blocks has to vary.
    fn capture_text(len: usize) -> String {
        let mut state = 0x243F_6A88_85A3_08D3_u64;
        let mut out = String::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push(char::from(b'a' + (state % 26) as u8));
        }
        out
    }

    /// A v3 manifest capture over two blocks, with a store that can serve its
    /// rows and witnesses, break either, or fail outright.
    fn manifest_capture() -> (String, Vec<(String, String, Vec<u8>)>) {
        let mut body = serde_json::json!({"model": "m", "stream": true});
        body["padding"] = serde_json::json!(capture_text(6000));
        let canonical = canonical_json_string(&body).unwrap();
        let chunks = encoding::chunk_capture_body(&canonical);
        assert!(chunks.len() > 1, "fixture must span several blocks");
        let blocks = chunks
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| (format!("witness-{index}"), chunk.content_key, chunk.bytes))
            .collect::<Vec<_>>();
        let entries = blocks
            .iter()
            .enumerate()
            .map(|(index, (witness, key, bytes))| encoding::ManifestEntry {
                doc_id: format!("block-{index}"),
                content_key: key.clone(),
                field_commit_cid: witness.clone(),
                byte_len: u64::try_from(bytes.len()).unwrap(),
            })
            .collect::<Vec<_>>();
        let manifest = encoding::encode_manifest(&entries).unwrap();
        let stored = encoding::encode_container(&manifest, &manifest).unwrap();
        (stored, blocks)
    }

    /// What one block document answers with: its payload bytes and the payload
    /// field commit the store reports for it.
    type BlockAnswer = Option<(Vec<u8>, String)>;

    /// The `(alias, docID)` pairs of a batched `_commits` query, so the fake
    /// stores can answer the commit read for every block document it names.
    fn commit_query_aliases(query: &str) -> Vec<(String, String)> {
        let parts = query.split(": _commits(docID: \"").collect::<Vec<_>>();
        parts
            .windows(2)
            .filter_map(|pair| {
                let alias = pair[0].split_whitespace().last()?;
                let (doc_id, _) = pair[1].split_once('"')?;
                Some((alias.to_owned(), doc_id.to_owned()))
            })
            .collect()
    }

    /// Answer a batched `_commits` query with one payload commit per alias.
    fn commit_response(
        aliases: Vec<(String, String)>,
        cid_of: impl Fn(&str) -> Option<String>,
    ) -> Value {
        let data = aliases
            .into_iter()
            .map(|(alias, doc_id)| {
                let commits = cid_of(&doc_id)
                    .map(|cid| serde_json::json!([{ "cid": cid, "height": 1, "fieldName": "payload" }]))
                    .unwrap_or_else(|| serde_json::json!([]));
                (alias, commits)
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({ "data": data })
    }

    /// A block store whose failures the test selects: `fail_queries` turns every
    /// read into a store error, `None` reports no row for a block, and a pair
    /// reports a payload and commit the manifest may disagree with.
    struct ManifestStore {
        fail_queries: bool,
        blocks: Vec<(String, String, Vec<u8>)>,
        answers: std::collections::BTreeMap<usize, BlockAnswer>,
    }

    impl ManifestStore {
        fn healthy(blocks: &[(String, String, Vec<u8>)]) -> Self {
            Self {
                fail_queries: false,
                blocks: blocks.to_vec(),
                answers: std::collections::BTreeMap::new(),
            }
        }

        fn broken(
            blocks: &[(String, String, Vec<u8>)],
            answers: std::collections::BTreeMap<usize, BlockAnswer>,
        ) -> Self {
            Self {
                fail_queries: false,
                blocks: blocks.to_vec(),
                answers,
            }
        }
    }

    #[async_trait::async_trait]
    impl CaptureBaseReader for ManifestStore {
        async fn execute_capture_query(&self, query: &str) -> Result<Value> {
            if self.fail_queries {
                anyhow::bail!("store unavailable");
            }
            let aliases = commit_query_aliases(query);
            if !aliases.is_empty() {
                return Ok(commit_response(aliases, |doc_id| {
                    let index = doc_id.strip_prefix("block-")?.parse::<usize>().ok()?;
                    Some(
                        self.answers
                            .get(&index)
                            .cloned()
                            .flatten()
                            .map(|(_, cid)| cid)
                            .unwrap_or_else(|| self.blocks[index].0.clone()),
                    )
                }));
            }
            if !query.contains("RenderedRequestBlock") {
                return Ok(serde_json::json!({"data": {"RenderedRequest": []}}));
            }
            let rows = self
                .blocks
                .iter()
                .enumerate()
                .filter_map(|(index, (_, key, bytes))| {
                    let answer = self
                        .answers
                        .get(&index)
                        .cloned()
                        .unwrap_or_else(|| Some((bytes.clone(), String::new())));
                    let (bytes, _) = answer?;
                    Some(serde_json::json!({
                        "_docID": format!("block-{index}"),
                        "payload": String::from_utf8(bytes.clone()).unwrap(),
                        "byte_len": bytes.len(),
                        "content_key": key,
                    }))
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({"data": {"RenderedRequestBlock": rows}}))
        }
    }

    /// The manifest arm of the split `base_resolution_distinguishes_store_reads
    /// _from_missing_bases` pin: a store read failure while fetching blocks is
    /// typed so replay propagates it, while an absent block, a block rewritten
    /// under another commit and a short block read are verification failures
    /// that drop only the affected turn.
    #[tokio::test]
    async fn manifest_resolution_distinguishes_store_reads_from_broken_blocks() {
        use std::collections::BTreeMap;
        let (stored, blocks) = manifest_capture();
        let byte_len = blocks[0].2.len();
        let version = gents_protocol::rendered_request::CAPTURE_VERSION;

        let mut failing = ManifestStore::healthy(&blocks);
        failing.fail_queries = true;
        let error =
            decode_capture_json_from(&failing, version, &stored, CapturePayloadKind::RequestBody)
                .await
                .expect_err("a failed store read cannot decode");
        assert!(
            error.downcast_ref::<CaptureStoreReadError>().is_some(),
            "{error:#}"
        );

        let cases: [(&str, BTreeMap<usize, BlockAnswer>); 3] = [
            ("an absent block document", BTreeMap::from([(0usize, None)])),
            (
                "a block rewritten under another commit",
                BTreeMap::from([(0usize, Some((blocks[0].2.clone(), "changed".into())))]),
            ),
            (
                "a short block read",
                BTreeMap::from([(
                    0usize,
                    Some((blocks[0].2[..byte_len - 1].to_vec(), blocks[0].0.clone())),
                )]),
            ),
        ];
        for (label, answers) in cases {
            let store = ManifestStore::broken(&blocks, answers);
            let error =
                decode_capture_json_from(&store, version, &stored, CapturePayloadKind::RequestBody)
                    .await
                    .err()
                    .unwrap_or_else(|| panic!("{label} must fail closed"));
            assert!(
                error.downcast_ref::<CaptureStoreReadError>().is_none(),
                "{label} is a verification failure, not a store failure: {error:#}"
            );
        }

        let healthy = ManifestStore::healthy(&blocks);
        let mut cache = CaptureReadCache::default();
        for kind in [
            CapturePayloadKind::RequestBody,
            CapturePayloadKind::ProvenancePayload,
        ] {
            let decoded =
                decode_capture_json_from_cached(&healthy, version, &stored, kind, &mut cache)
                    .await
                    .unwrap();
            assert_eq!(decoded["model"], serde_json::json!("m"));
        }
    }

    /// A block store that answers every named document with a fixed payload
    /// and commit, so one manifest entry can name a wrong-content document
    /// under a content key another manifest references correctly.
    struct MisreferencingBlockStore {
        docs: Vec<(String, String, String)>,
    }

    #[async_trait::async_trait]
    impl CaptureBaseReader for MisreferencingBlockStore {
        async fn execute_capture_query(&self, query: &str) -> Result<Value> {
            let aliases = commit_query_aliases(query);
            if !aliases.is_empty() {
                return Ok(commit_response(aliases, |doc_id| {
                    self.docs
                        .iter()
                        .find(|(named, _, _)| named == doc_id)
                        .map(|(_, _, cid)| cid.clone())
                }));
            }
            if !query.contains("RenderedRequestBlock") {
                return Ok(serde_json::json!({"data": {"RenderedRequest": []}}));
            }
            let rows = self
                .docs
                .iter()
                .map(|(doc_id, payload, _)| {
                    serde_json::json!({
                        "_docID": doc_id,
                        "payload": payload,
                        "byte_len": payload.len(),
                    })
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({"data": {"RenderedRequestBlock": rows}}))
        }
    }

    /// The cache-poisoning regression: a manifest entry naming a wrong-content
    /// document under a content key must fail as that capture's own
    /// verification error without ever entering the block cache — a cached
    /// mismatch would fail the witness check of a later, correct capture of
    /// the same content and drop a turn nothing is wrong with.
    #[tokio::test]
    async fn a_misreferenced_block_entry_drops_only_its_own_turn() {
        let mut body = serde_json::json!({"model": "m", "stream": true});
        body["padding"] = serde_json::json!(capture_text(6000));
        let canonical = canonical_json_string(&body).unwrap();
        let chunks = encoding::chunk_capture_body(&canonical);
        assert!(chunks.len() > 1, "fixture must span several blocks");

        let wrong_payload = "z".repeat(chunks[0].bytes.len());
        assert_ne!(
            encoding::block_content_key(wrong_payload.as_bytes()),
            chunks[0].content_key
        );
        let mut docs = vec![(
            "block-wrong".to_string(),
            wrong_payload,
            "wrong-commit".to_string(),
        )];
        let entries = chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                docs.push((
                    format!("block-{index}"),
                    String::from_utf8(chunk.bytes.clone()).unwrap(),
                    format!("witness-{index}"),
                ));
                encoding::ManifestEntry {
                    doc_id: format!("block-{index}"),
                    content_key: chunk.content_key.clone(),
                    field_commit_cid: format!("witness-{index}"),
                    byte_len: u64::try_from(chunk.bytes.len()).unwrap(),
                }
            })
            .collect::<Vec<_>>();
        let store = MisreferencingBlockStore { docs };

        // Turn N's first entry names the wrong-content document under the
        // correct content key; turn M references the same key correctly.
        let mut misreferenced = entries.clone();
        misreferenced[0].doc_id = "block-wrong".into();
        misreferenced[0].field_commit_cid = "wrong-commit".into();
        let encode_turn = |entries: &[encoding::ManifestEntry]| {
            let manifest = encoding::encode_manifest(entries).unwrap();
            encoding::encode_container(&manifest, &manifest).unwrap()
        };
        let turn_n = encode_turn(&misreferenced);
        let turn_m = encode_turn(&entries);

        let version = gents_protocol::rendered_request::CAPTURE_VERSION;
        let mut cache = CaptureReadCache::default();
        let error = decode_capture_json_from_cached(
            &store,
            version,
            &turn_n,
            CapturePayloadKind::RequestBody,
            &mut cache,
        )
        .await
        .err()
        .expect("the misreferencing turn must fail closed");
        assert!(
            error.downcast_ref::<CaptureStoreReadError>().is_none(),
            "a misreferenced block is a verification failure, not a store failure: {error:#}"
        );
        assert!(
            error
                .to_string()
                .contains("does not hash to its content key"),
            "{error:#}"
        );

        let replayed = decode_capture_json_from_cached(
            &store,
            version,
            &turn_m,
            CapturePayloadKind::RequestBody,
            &mut cache,
        )
        .await
        .expect("the neighboring turn must still replay through the shared cache");
        assert_eq!(replayed, body);

        // The cache now holds every correct block. A manifest naming an absent
        // document under a cached block's content key, witness and length
        // still fails: the cache answers documents, not content.
        let mut absent = entries.clone();
        absent[0].doc_id = "block-absent".into();
        let error = decode_capture_json_from_cached(
            &store,
            version,
            &encode_turn(&absent),
            CapturePayloadKind::RequestBody,
            &mut cache,
        )
        .await
        .err()
        .expect("a manifest naming an absent document must fail closed");
        assert!(
            error.downcast_ref::<CaptureStoreReadError>().is_none(),
            "an absent block is a verification failure, not a store failure: {error:#}"
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
        |_| anyhow::bail!("capture manifest requires block resolution"),
    )
}

/// A store read that failed while resolving a capture's delta chain or block
/// manifest. It says nothing about the capture itself, so replay must not
/// treat it as an unverifiable capture.
#[derive(Debug, thiserror::Error)]
#[error("reading a rendered-request capture dependency from the store: {0:#}")]
pub struct CaptureStoreReadError(pub anyhow::Error);

#[async_trait::async_trait]
trait CaptureBaseReader {
    async fn execute_capture_query(&self, query: &str) -> Result<Value>;
}

#[async_trait::async_trait]
impl CaptureBaseReader for crate::config_client::ConfigAccess {
    async fn execute_capture_query(&self, query: &str) -> Result<Value> {
        self.execute(query).await
    }
}

#[async_trait::async_trait]
impl CaptureBaseReader for defra_node::EmbeddedNode {
    async fn execute_capture_query(&self, query: &str) -> Result<Value> {
        let response = crate::graphql::graphql_with_transaction_retry(
            self,
            query,
            "reading rendered-request capture dependency",
        )
        .await?;
        crate::graphql::ensure_no_errors(&response, "reading rendered-request capture dependency")?;
        Ok(serde_json::json!({"data": response.data}))
    }
}

type CaptureBaseCache = std::collections::BTreeMap<String, (Value, String)>;

/// One block document already read and witnessed, keyed by its document id:
/// the manifest pins a document, so a cached observation answers only entries
/// naming that document, and each entry still checks the content key it was
/// read under. Blocks are immutable, so a positive observation stays valid for
/// the life of the cache.
struct CachedCaptureBlock {
    content_key: String,
    bytes: Vec<u8>,
    field_commit_cid: String,
}

/// Witnessed positive base and block observations for one replay resolution.
/// Discard between resolutions: a later read must observe edited or replicated
/// dependencies.
#[derive(Default)]
pub(crate) struct CaptureReadCache {
    bases: CaptureBaseCache,
    blocks: std::collections::BTreeMap<String, CachedCaptureBlock>,
}

async fn decode_capture_json_from_cached<R: CaptureBaseReader + Sync>(
    reader: &R,
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
    cache: &mut CaptureReadCache,
) -> Result<Value> {
    let mut next_version = capture_version;
    let mut next_stored = stored.to_owned();
    let mut bases = std::collections::BTreeMap::new();
    for _ in 0..=encoding::MAX_DELTA_DEPTH {
        match encoding::decode_capture_record(next_version, &next_stored, kind)? {
            encoding::DecodedRecord::Legacy(_) | encoding::DecodedRecord::Full(_) => break,
            encoding::DecodedRecord::Manifest { blocks } => {
                read_manifest_blocks(reader, &blocks, &mut cache.blocks).await?;
                break;
            }
            encoding::DecodedRecord::Delta { base, .. } => {
                if !cache.bases.contains_key(&base.doc_id) {
                    let query = format!(
                        r#"{{ RenderedRequest(filter: {{_docID: {{_eq: "{doc_id}"}}}}, limit: 2) {{
                            capture_version agent_did requester_did session_id source capture_scope request_json
                        }}
                        _commits(docID: "{doc_id}") {{ cid height fieldName }}
                        }}"#,
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
                    let actual = commits::select_field_commit(&response, "request_json")
                        .map_err(|error| anyhow::Error::new(CaptureStoreReadError(error)))?
                        .context("rendered-request delta base lacks field commit")?
                        .cid;
                    cache
                        .bases
                        .insert(base.doc_id.clone(), (row.clone(), actual));
                }
                let (row, actual) = cache
                    .bases
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
    encoding::resolve_capture_with(
        capture_version,
        stored,
        kind,
        |base| {
            bases
                .remove(&(base.doc_id.clone(), base.field_commit_cid.clone()))
                .context("capture delta chain exceeds maximum depth or contains a cycle")
        },
        |entry| {
            let block = cache
                .blocks
                .get(&entry.doc_id)
                .context("capture manifest block was not read")?;
            anyhow::ensure!(
                block.content_key == entry.content_key,
                "capture manifest block {} was read under another content key",
                entry.doc_id
            );
            Ok((block.bytes.clone(), block.field_commit_cid.clone()))
        },
    )
}

/// Read the block documents a manifest names that are not cached yet: one
/// batched row query plus batched field-commit reads.
///
/// A failed query is a [`CaptureStoreReadError`]: it says the store is broken,
/// not that the capture is unverifiable. A block document that is simply
/// absent is left uncached, so resolution reports it as a verification failure
/// and replay drops only the affected turn. A document whose payload does not
/// hash to the entry's content key is the same kind of verification failure
/// for the capture that named it, and is refused before the cache.
async fn read_manifest_blocks<R: CaptureBaseReader + Sync>(
    reader: &R,
    entries: &[encoding::ManifestEntry],
    cache: &mut std::collections::BTreeMap<String, CachedCaptureBlock>,
) -> Result<()> {
    anyhow::ensure!(
        entries.len() <= encoding::MAX_MANIFEST_BLOCKS,
        "capture manifest names {} blocks, above the {} the reader resolves",
        entries.len(),
        encoding::MAX_MANIFEST_BLOCKS
    );
    let mut unread = std::collections::BTreeMap::new();
    for entry in entries {
        if !cache.contains_key(&entry.doc_id) {
            unread.entry(entry.doc_id.as_str()).or_insert(entry);
        }
    }
    if unread.is_empty() {
        return Ok(());
    }
    let doc_ids = unread
        .keys()
        .map(|doc_id| format!("\"{}\"", crate::graphql::escape_graphql_string(doc_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{ RenderedRequestBlock(filter: {{_docID: {{_in: [{doc_ids}] }} }}) {{
            _docID payload byte_len
        }} }}"#,
    );
    let response = reader
        .execute_capture_query(&query)
        .await
        .map_err(|error| anyhow::Error::new(CaptureStoreReadError(error)))?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("RenderedRequestBlock"))
        .and_then(Value::as_array)
        .context("reading capture manifest blocks returned an unexpected shape")?;
    let mut found = Vec::new();
    for row in rows {
        let doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .context("capture manifest block row lacks _docID")?;
        let Some(entry) = unread.remove(doc_id) else {
            continue;
        };
        let payload = row
            .get("payload")
            .and_then(Value::as_str)
            .context("capture manifest block row lacks payload")?;
        let byte_len = row
            .get("byte_len")
            .and_then(Value::as_u64)
            .context("capture manifest block row lacks byte_len")?;
        anyhow::ensure!(
            byte_len == entry.byte_len,
            "capture manifest block {} stores byte_len {byte_len}, manifest pins {}",
            entry.doc_id,
            entry.byte_len
        );
        anyhow::ensure!(
            encoding::block_content_key(payload.as_bytes()) == entry.content_key,
            "capture manifest block {} stores a payload that does not hash to its content key",
            entry.doc_id
        );
        found.push((entry, payload.as_bytes().to_vec()));
    }
    let found_doc_ids = found
        .iter()
        .map(|(entry, _)| entry.doc_id.clone())
        .collect::<Vec<_>>();
    let witnesses = commits::field_commits(&found_doc_ids, "payload", |query| async move {
        reader.execute_capture_query(&query).await
    })
    .await
    .map_err(|error| anyhow::Error::new(CaptureStoreReadError(error)))?;
    for ((entry, bytes), commit) in found.into_iter().zip(witnesses) {
        let commit = commit.with_context(|| {
            format!(
                "capture manifest block {} lacks a payload field commit",
                entry.doc_id
            )
        })?;
        cache.insert(
            entry.doc_id.clone(),
            CachedCaptureBlock {
                content_key: entry.content_key.clone(),
                bytes,
                field_commit_cid: commit.cid,
            },
        );
    }
    Ok(())
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
        &mut CaptureReadCache::default(),
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
    let mut cache = CaptureReadCache::default();
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

/// Decode both payloads of one capture through the same witnessed resolver,
/// for callers that already own the embedded DefraDB node.
pub async fn decode_capture_pair_embedded(
    node: &defra_node::EmbeddedNode,
    capture_version: u32,
    stored: &str,
) -> Result<(Value, Value)> {
    decode_capture_pair_from(node, capture_version, stored).await
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

pub(crate) async fn decode_capture_json_embedded_cached(
    node: &defra_node::EmbeddedNode,
    capture_version: u32,
    stored: &str,
    kind: CapturePayloadKind,
    cache: &mut CaptureReadCache,
) -> Result<Value> {
    decode_capture_json_from_cached(node, capture_version, stored, kind, cache).await
}
