use super::*;
use crate::store::identity::NodeIdentity;
use serde_json::{json, Value};

fn fixture_identities() -> &'static (Arc<crate::KeyIdentity>, Arc<dyn NodeIdentity>) {
    static IDENTITIES: std::sync::OnceLock<(Arc<crate::KeyIdentity>, Arc<dyn NodeIdentity>)> =
        std::sync::OnceLock::new();
    IDENTITIES.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let root = Arc::new(
            crate::KeyIdentity::load_or_create(directory.path().join("root.key"), None).unwrap(),
        );
        let binding = crate::store::identity::provision_agent_identity(root.as_ref()).unwrap();
        let agent =
            crate::store::identity::resolve_agent_identity(root.as_ref(), &binding).unwrap();
        (root, agent)
    })
}

struct CaptureFixture {
    db: crate::support::TestDb,
    _user: crate::KeyIdentity,
    agent: Arc<dyn NodeIdentity>,
    lifecycle: crate::request::lifecycle::RequestLifecycle,
}

impl CaptureFixture {
    async fn new() -> Self {
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        let (db, user, client, binding) =
            crate::test_support::signed_test_node("rendered-capture").await;
        let agent =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "request",
            db.node_identity.did(),
            user.did(),
            agent.did(),
            "session",
            "capture",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let mut lifecycle =
            crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
                &db.node,
                &client,
                db.node_identity.clone(),
                &user,
                agent.as_ref(),
                create,
            )
            .await
            .unwrap();
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
            db.node.clone(),
            &lifecycle,
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        Self {
            db,
            _user: user,
            agent,
            lifecycle,
        }
    }

    fn access(&self) -> crate::ConfigAccess {
        crate::ConfigAccess::local_managed_agent(
            self.db.node.clone(),
            identity::Did::new(self.agent.did()).unwrap(),
            self.agent.as_ref(),
        )
        .unwrap()
    }

    fn sink(&self) -> DefraRenderedRequestSink {
        DefraRenderedRequestSink::new_managed(self.db.node.clone(), self.agent.as_ref()).unwrap()
    }

    fn turn(&self, turn_index: usize, request_json: Value) -> RenderedCompletionRequest {
        let mut rendered = rendered_turn_for(
            self.db.node_identity.did(),
            self.agent.did(),
            turn_index,
            request_json,
        );
        let request = self.lifecycle.request();
        rendered.node_did = request.node_did.clone();
        rendered.requester_did = request.requester_did.clone().unwrap();
        rendered.agent_did = request.agent_did.clone();
        rendered.session_id = request.session_id.clone();
        rendered.request_doc_id = request.doc_id.clone();
        rendered.request_id = request.request_id.clone();
        rendered.request_commit_cid = self.lifecycle.request_commit_cid().unwrap().into();
        rendered.capture_key = super::super::capture_key(
            &rendered.node_did,
            &rendered.session_id,
            &rendered.request_doc_id,
            &rendered.capture_scope,
            turn_index,
            0,
        )
        .unwrap();
        rendered
    }
}

fn fixture_sink(node: &Arc<EmbeddedNode>) -> DefraRenderedRequestSink {
    DefraRenderedRequestSink {
        node: node.clone(),
        agent_signer: Some(fixture_identities().1.managed_agent_signer().unwrap()),
    }
}

#[tokio::test]
async fn failed_capture_publication_rolls_back_bound_blocks_and_record() {
    use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};

    let (root, agent) = fixture_identities();
    let sibling_binding = crate::store::identity::provision_agent_identity(root.as_ref()).unwrap();
    let sibling =
        crate::store::identity::resolve_agent_identity(root.as_ref(), &sibling_binding).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(root.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await
        .unwrap();
    for (name, schema) in [
        ("RenderedRequest", gents_protocol::schemas::RENDERED_REQUEST),
        (
            "RenderedRequestBlock",
            gents_protocol::schemas::RENDERED_REQUEST_BLOCK,
        ),
    ] {
        let bound = if schema.contains("@policy(") {
            schema.to_owned()
        } else {
            schema.replacen(
                &format!("type {name}"),
                &format!("type {name} @policy(id: \"{policy}\", resource: \"children\")"),
                1,
            )
        };
        node.add_collection(&bound).await.unwrap();
    }
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    // Production projects the AgentBaseline from each live Agent document; this
    // fixture has no projection pass, so the node admin grants it to both the
    // capturing Agent and its sibling, which the ownership policy must still refuse.
    let node_admin = ConfigAccess::local_as(node.clone(), root.did().parse().unwrap());
    for executor in [agent.did(), sibling.did()] {
        crate::store::identity::GrantSet::AgentBaseline
            .grant(&node_admin, &executor.parse().unwrap())
            .await
            .unwrap();
    }
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    access
        .transact_write("test.capture.session", |txn| {
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    "session",
                    root.did(),
                    agent.did(),
                    None,
                    None,
                    None,
                    "2026-01-01T00:00:00Z",
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let session = access
        .execute(r#"{ AgentSession { _docID } }"#)
        .await
        .unwrap()["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    access
        .add_actor_relationship(
            "AgentSession",
            &session,
            "reader",
            &root.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let reader = ConfigAccess::local_as(node.clone(), root.did().parse().unwrap());
    let sibling_access = ConfigAccess::local_managed_agent(
        node.clone(),
        sibling.did().parse().unwrap(),
        sibling.as_ref(),
    )
    .unwrap();
    let sink = fixture_sink(&node);
    sink.capture(rendered_turn(
        0,
        json!({"messages": [capture_text(1, 8192)]}),
    ))
    .await
    .unwrap();
    let query = "{ RenderedRequest { _docID } RenderedRequestBlock { _docID } }";
    let before = reader.execute(query).await.unwrap();
    for collection in ["RenderedRequest", "RenderedRequestBlock"] {
        assert!(!before["data"][collection].as_array().unwrap().is_empty());
        assert!(
            sibling_access.execute(query).await.unwrap()["data"][collection]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let failed = rendered_turn(1, json!({"messages": [capture_text(2, 8192)]}));
    let (result, mutations) =
        ConfigApplyTxn::with_successful_mutation_failure_at(Some(1), sink.capture(failed)).await;
    assert!(result.is_err());
    assert_eq!(mutations, 1);
    assert_eq!(reader.execute(query).await.unwrap(), before);
    assert_eq!(access.execute(query).await.unwrap(), before);
    node.shutdown().await.unwrap();
}

#[test]
fn generated_capture_block_scope_cases_use_physical_session_identity() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().capture_block_scope_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let field = |name: &str| case[name].as_str().expect("block scope case field");
        let first = BlockScope {
            session_doc_id: field("first_session").into(),
            agent_did: String::new(),
        };
        let second = BlockScope {
            session_doc_id: field("second_session").into(),
            agent_did: String::new(),
        };
        let left = first.key(field("first_digest"));
        let right = second.key(field("second_digest"));
        assert_eq!(
            left == right,
            case["same_storage_identity"].as_bool().unwrap()
        );
    }
}

fn capture_text(seed: u64, len: usize) -> String {
    let mut state = seed;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(char::from(b'a' + (state % 26) as u8));
    }
    out
}

fn rendered_turn(turn_index: usize, request_json: Value) -> RenderedCompletionRequest {
    let (root, agent) = fixture_identities();
    rendered_turn_for(root.did(), agent.did(), turn_index, request_json)
}

fn rendered_turn_for(
    node_did: &str,
    agent_did: &str,
    turn_index: usize,
    request_json: Value,
) -> RenderedCompletionRequest {
    let capture_scope = "inference.1".to_string();
    let assembly_trace = super::super::AssemblyTrace::from_effective_messages(
        super::super::AssemblyBuildPath::Budgeted,
        Vec::new(),
    );
    RenderedCompletionRequest {
        capture_key: super::super::capture_key(
            node_did,
            "session",
            "",
            &capture_scope,
            turn_index,
            0,
        )
        .unwrap(),
        capture_version: gents_protocol::rendered_request::CAPTURE_VERSION,
        request_doc_id: String::new(),
        request_commit_cid: String::new(),
        request_id: "request".into(),
        capture_scope: capture_scope.clone(),
        turn_index,
        attempt: 0,
        node_did: node_did.into(),
        requester_did: String::new(),
        agent_did: agent_did.to_owned(),
        session_id: "session".into(),
        model_name: "model".into(),
        source: super::super::RenderedRequestSource::OpenAiChatCompletions,
        request_json,
        messages_json: json!([]),
        tools_json: json!([]),
        tool_choice_json: Value::Null,
        sampling_json: Value::Null,
        provenance_json: serde_json::to_value(super::super::ProvenanceManifest::captured_only(
            capture_scope,
            None,
            None,
            assembly_trace.clone(),
        ))
        .unwrap(),
        provenance_payload_json: serde_json::to_value(&assembly_trace).unwrap(),
        assembly_trace,
    }
}

async fn block_rows(access: &crate::ConfigAccess) -> Vec<Value> {
    let response = access
        .execute(r#"{ RenderedRequestBlock { _docID content_key } }"#)
        .await
        .unwrap();
    response["data"]["RenderedRequestBlock"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn stored_capture(access: &crate::ConfigAccess, capture_key: &str) -> Value {
    let response = access.execute(&format!(r#"{{ RenderedRequest(filter: {{ capture_key: {{ _eq: "{}" }} }}, limit: 2) {{ capture_version request_json }} }}"#, escape_graphql_string(capture_key))).await.unwrap();
    let rows = response["data"]["RenderedRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "exactly one capture row per key");
    rows[0].clone()
}

#[tokio::test]
async fn capture_and_context_accounting_commit_together() {
    use gents_protocol::rendered_request::{
        AdmissionJoin, ContextAccounting, ContextCompactionReason, ContextInputComponents,
        ProvenanceManifest, CONTEXT_ACCOUNTING_VERSION,
    };

    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();
    let session = access.execute("{ AgentSession { _docID } }").await.unwrap()["data"]
        ["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    access.transact_write("test.create_accounting_call", |txn| {
        let session = &session;
        Box::pin(async move {
            let response = txn.execute(r#"mutation { create_InferenceCall(input: { call_id: "accounting", request_id: "request", call_state: "running" }) { _docID } }"#).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "InferenceCall")?;
            txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let with_accounting = |turn, call_id: &str, messages| {
        let mut rendered = fixture.turn(
            turn,
            json!({"model": "m", "messages": [{"role": "user", "content": "body"}]}),
        );
        rendered.assembly_trace.context_accounting = Some(ContextAccounting {
            accounting_version: CONTEXT_ACCOUNTING_VERSION,
            turn_index: turn,
            attempt: 0,
            estimator: "serialized_json_bytes_div_4_v1".into(),
            components: ContextInputComponents {
                messages,
                documents: 0,
                tool_schemas: 0,
                additional_parameters: 0,
                output_schema: 0,
            },
            estimated_input_tokens: messages,
            raw_estimated_input_tokens: None,
            input_calibration_basis_points: None,
            anchored_input_tokens: None,
            context_window: 100,
            compaction_threshold_basis_points: 7500,
            compaction_threshold_tokens: 75,
            configured_max_output_tokens: Some(10),
            effective_max_output_tokens: Some(10),
            compaction_reason: ContextCompactionReason::BelowThreshold,
            pre_compaction_input_tokens: None,
        });
        rendered.provenance_payload_json = serde_json::to_value(&rendered.assembly_trace).unwrap();
        rendered.provenance_json = serde_json::to_value(ProvenanceManifest::captured_only(
            rendered.capture_scope.clone(),
            None,
            Some(AdmissionJoin {
                call_id: call_id.into(),
                call_seq: 1,
            }),
            rendered.assembly_trace.clone(),
        ))
        .unwrap();
        rendered
    };
    let first = with_accounting(0, "accounting", 1);
    sink.capture(first.clone()).await.unwrap();
    sink.capture(first.clone()).await.unwrap();
    let call = access.execute(r#"{ InferenceCall(filter: {call_id: {_eq: "accounting"}}) { context_accounting_json } }"#).await.unwrap();
    let stored: Value = serde_json::from_str(
        call["data"]["InferenceCall"][0]["context_accounting_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        stored,
        serde_json::to_value(first.assembly_trace.context_accounting.as_ref().unwrap()).unwrap()
    );
    let original_blocks = block_rows(&access).await.len();
    for rejected in [
        with_accounting(1, "accounting", 2),
        with_accounting(2, "missing", 1),
    ] {
        assert!(sink.capture(rejected).await.is_err());
        let captures = access
            .execute("{ RenderedRequest { _docID } }")
            .await
            .unwrap();
        assert_eq!(
            captures["data"]["RenderedRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(block_rows(&access).await.len(), original_blocks);
    }
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn capture_writes_blocks_and_a_manifest_that_decode_back() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();
    let rendered = fixture.turn(
        0,
        json!({"model": "m", "messages": [{"role": "user", "content": capture_text(1, 9 * 1024)}]}),
    );
    sink.capture(rendered.clone()).await.expect("capture");

    let row = stored_capture(&access, &rendered.capture_key).await;
    assert_eq!(
        row["capture_version"].as_u64(),
        Some(u64::from(gents_protocol::rendered_request::CAPTURE_VERSION))
    );
    let stored = row["request_json"].as_str().unwrap();
    let container: Value = serde_json::from_str(stored).unwrap();
    assert_eq!(container["request_body"]["kind"], "tree");
    assert_eq!(container["provenance_payload"]["kind"], "tree");
    let blocks = block_rows(&access).await;
    assert!(
        blocks.len() > 1,
        "a body spanning several blocks must be stored as blocks"
    );

    let (request, provenance) = super::super::decode_capture_pair(
        &access,
        gents_protocol::rendered_request::CAPTURE_VERSION,
        stored,
    )
    .await
    .expect("decode captured pair");
    assert_eq!(
        request,
        super::super::canonical_json(&rendered.request_json)
    );
    assert_eq!(
        provenance,
        super::super::canonical_json(&rendered.provenance_payload_json)
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn repeated_tool_output_is_stored_once_across_wire_and_native_payloads() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();
    let text = capture_text(73, 8 * 1024);
    let mut rendered = fixture.turn(
        0,
        json!({
            "messages": [{"role": "tool", "content": text}]
        }),
    );
    rendered.provenance_payload_json = json!({
        "effective_messages": [{"content": [{"text": text}]}],
        "threaded_tool_results": [{"content": [{"text": text}]}]
    });
    sink.capture(rendered.clone()).await.unwrap();
    let rows = block_rows(&access).await;
    let text_chunks =
        super::super::encoding::chunk_capture_body(&serde_json::to_string(&text).unwrap());
    for chunk in text_chunks {
        assert_eq!(
            rows.iter()
                .filter(|row| row["content_key"] == chunk.content_key)
                .count(),
            1
        );
    }
    let row = stored_capture(&access, &rendered.capture_key).await;
    let decoded = super::super::decode_capture_pair(
        &access,
        super::super::CAPTURE_VERSION,
        row["request_json"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        decoded,
        (rendered.request_json, rendered.provenance_payload_json)
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_shared_prefix_writes_only_new_blocks_and_a_small_manifest() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();

    let prefix = capture_text(7, 24 * 1024);
    let first = fixture.turn(
        0,
        json!({"model": "m", "messages": [{"role": "user", "content": prefix.clone()}]}),
    );
    sink.capture(first.clone()).await.expect("first capture");
    let first_blocks = block_rows(&access).await;

    let mut second_messages = first.request_json["messages"].clone();
    second_messages
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "assistant", "content": capture_text(9, 1024)}));
    let second = fixture.turn(1, json!({"model": "m", "messages": second_messages}));
    sink.capture(second.clone()).await.expect("second capture");
    let second_blocks = block_rows(&access).await;

    assert!(
        second_blocks.len() > first_blocks.len(),
        "the new message must add blocks"
    );
    let added: std::collections::BTreeSet<String> = second_blocks
        .iter()
        .map(|row| row["content_key"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .difference(
            &first_blocks
                .iter()
                .map(|row| row["content_key"].as_str().unwrap().to_owned())
                .collect(),
        )
        .cloned()
        .collect();
    assert!(
        (added.len() as f64) < 0.4 * second_blocks.len() as f64,
        "{} new of {} total blocks; the shared prefix must be reused",
        added.len(),
        second_blocks.len()
    );

    let row = stored_capture(&access, &second.capture_key).await;
    let stored = row["request_json"].as_str().unwrap();
    assert!(
        stored.len() < 3 * prefix.len() / 4,
        "the capture row must not repeat the conversation: {} bytes for a {} byte body",
        stored.len(),
        prefix.len()
    );
    assert_eq!(
        super::super::decode_capture_json(
            &access,
            gents_protocol::rendered_request::CAPTURE_VERSION,
            stored,
            super::super::CapturePayloadKind::RequestBody,
        )
        .await
        .unwrap(),
        super::super::canonical_json(&second.request_json)
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_reused_block_pins_the_existing_row_field_commit() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();

    let body = capture_text(11, 12 * 1024);
    let first = fixture.turn(
        0,
        json!({"model": "m", "messages": [{"role": "user", "content": body}]}),
    );
    sink.capture(first).await.expect("first capture");
    let first_row = stored_capture(&access, &fixture.turn(0, json!({})).capture_key).await;
    let first_entries = manifest_entries(
        &access,
        first_row["request_json"].as_str().unwrap(),
        "request_body",
    )
    .await;
    let first_blocks = block_rows(&access).await;

    let mut messages = json!([{"role": "user", "content": body}]);
    messages
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "assistant", "content": capture_text(13, 512)}));
    let second = fixture.turn(1, json!({"model": "m", "messages": messages}));
    sink.capture(second.clone()).await.expect("second capture");

    let second_row = stored_capture(&access, &second.capture_key).await;
    let second_entries = manifest_entries(
        &access,
        second_row["request_json"].as_str().unwrap(),
        "request_body",
    )
    .await;
    let by_key = |entries: &[(String, String, String)]| {
        entries
            .iter()
            .map(|(doc_id, key, cid)| (key.clone(), (doc_id.clone(), cid.clone())))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let first_map = by_key(&first_entries);
    let second_map = by_key(&second_entries);
    let shared = second_map
        .keys()
        .filter(|key| first_map.contains_key(*key))
        .count();
    assert!(shared > 0, "the turns must share blocks");
    for (key, (doc_id, cid)) in &second_map {
        if let Some((first_doc_id, first_cid)) = first_map.get(key) {
            assert_eq!(
                doc_id, first_doc_id,
                "shared block {key} must reuse the row"
            );
            assert_eq!(
                cid, first_cid,
                "shared block {key} must pin the existing witness"
            );
        }
    }
    assert!(block_rows(&access).await.len() > first_blocks.len());
    node.shutdown().await.expect("node shutdown");
}

async fn manifest_entries(
    access: &crate::ConfigAccess,
    stored: &str,
    payload: &str,
) -> Vec<(String, String, String)> {
    let kind = if payload == "request_body" {
        super::super::CapturePayloadKind::RequestBody
    } else {
        super::super::CapturePayloadKind::ProvenancePayload
    };
    let record = super::super::encoding::decode_capture_record(
        gents_protocol::rendered_request::CAPTURE_VERSION,
        stored,
        kind,
    )
    .unwrap();
    super::super::manifest_leaf_entries(access, record)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| (entry.doc_id, entry.content_key, entry.field_commit_cid))
        .collect()
}

#[tokio::test]
async fn deleted_blocks_never_publish_dangling_references() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();
    let body = json!({"messages":[{"role":"user","content":capture_text(89, 24 * 1024)}]});
    let first = fixture.turn(0, body.clone());
    sink.capture(first.clone()).await.unwrap();
    let row = stored_capture(&access, &first.capture_key).await;
    let record = super::super::encoding::decode_capture_record(
        super::super::CAPTURE_VERSION,
        row["request_json"].as_str().unwrap(),
        super::super::CapturePayloadKind::RequestBody,
    )
    .unwrap();
    let leaves = super::super::manifest_leaf_entries(&access, record)
        .await
        .unwrap();
    let id = escape_graphql_string(&leaves[0].doc_id);
    // Blocks are written, and so natively owned, by the capturing Agent. The
    // runtime grants it the node delete permission so its own access can
    // remove one out from under the next capture.
    let runtime_did = fixture.db.node_identity.did().to_owned();
    crate::ConfigAccess::local_as(node.clone(), identity::Did::new(runtime_did).unwrap())
        .grant_node_permission(
            &identity::Did::new(fixture.agent.did()).unwrap(),
            crate::store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    let deleted = access
        .write(
            "test.delete_capture_block",
            &format!(r#"mutation {{ delete_RenderedRequestBlock(docID: "{id}") {{ _docID }} }}"#),
        )
        .await
        .unwrap();
    assert!(crate::store::graphql::response_has_documents(
        &deleted["data"]["delete_RenderedRequestBlock"]
    ));
    let next = fixture.turn(1, body);
    let error = sink.capture(next.clone()).await.unwrap_err();
    assert!(format!("{error:#}").contains("deleted"), "{error:#}");
    assert!(sink.stored_fact(&next).await.unwrap().is_none());
    let reopened = fixture.sink();
    let error = reopened.capture(next.clone()).await.unwrap_err();
    assert!(format!("{error:#}").contains("deleted"), "{error:#}");
    assert!(reopened.stored_fact(&next).await.unwrap().is_none());
    let unrelated = fixture.turn(
        2,
        json!({"messages": [{"role": "user", "content": "fresh"}]}),
    );
    reopened.capture(unrelated.clone()).await.unwrap();
    assert!(reopened.stored_fact(&unrelated).await.unwrap().is_some());
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn shared_manifest_roots_stay_small_and_reconstruct_after_reopen() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let mut sink = fixture.sink();
    let mut messages = vec![json!({"role": "user", "content": capture_text(71, 256 * 1024)})];
    let mut first_pins = std::collections::BTreeSet::new();
    let mut flat_bytes = 0;
    let mut root_bytes = 0;
    let mut shared_nodes = std::collections::BTreeMap::new();
    for turn in 0..12 {
        if turn == 6 {
            sink = fixture.sink();
        }
        messages.push(json!({"role": "user", "content": capture_text(100 + turn as u64, 4096)}));
        let rendered = fixture.turn(turn, json!({"model":"m","messages":messages}));
        sink.capture(rendered.clone()).await.unwrap();
        let row = stored_capture(&access, &rendered.capture_key).await;
        let stored = row["request_json"].as_str().unwrap();
        assert!(
            stored.len() < 1200,
            "capture rewrote a growing manifest: {} bytes",
            stored.len()
        );
        root_bytes += stored.len();
        let mut cache = std::collections::BTreeMap::new();
        let mut flat = Vec::new();
        for kind in [
            super::super::CapturePayloadKind::RequestBody,
            super::super::CapturePayloadKind::ProvenancePayload,
        ] {
            let super::super::encoding::DecodedRecord::Tree(tree) =
                super::super::encoding::decode_capture_record(
                    super::super::CAPTURE_VERSION,
                    stored,
                    kind,
                )
                .unwrap()
            else {
                panic!("expected shared manifest");
            };
            let leaves = super::super::manifest_tree_leaves(&access, &tree, &mut cache)
                .await
                .unwrap();
            flat.push(super::super::encoding::encode_manifest(&leaves).unwrap());
        }
        flat_bytes += super::super::encoding::encode_container(&flat[0], &flat[1])
            .unwrap()
            .len();
        if turn > 0 {
            assert!(
                cache
                    .values()
                    .any(|block| shared_nodes.contains_key(&block.content_key)),
                "turn {turn} must reuse internal manifest nodes, including after reopening"
            );
        }
        for block in cache.values() {
            shared_nodes.insert(block.content_key.clone(), block.bytes.len());
        }
        let pins = manifest_entries(&access, stored, "request_body").await;
        if turn == 0 {
            first_pins.extend(pins.iter().cloned());
        } else {
            assert!(
                pins.iter().any(|pin| first_pins.contains(pin)),
                "turn {turn} must retain original immutable references"
            );
        }
        let decoded = super::super::decode_capture_pair(
            &access,
            gents_protocol::rendered_request::CAPTURE_VERSION,
            stored,
        )
        .await
        .unwrap();
        assert_eq!(
            decoded.0,
            super::super::canonical_json(&rendered.request_json)
        );
        assert_eq!(
            decoded.1,
            super::super::canonical_json(&rendered.provenance_payload_json)
        );
    }
    let shared_bytes = root_bytes + shared_nodes.values().sum::<usize>();
    assert!(
        shared_bytes < flat_bytes,
        "shared={shared_bytes}, flat={flat_bytes}"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn same_capture_key_cannot_rebind_its_transport_route() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let sink = fixture.sink();
    let mut first = fixture.turn(
        0,
        json!({"messages": [{"role": "user", "content": capture_text(3, 3 * 1024)}]}),
    );
    first.provenance_json["provider_family"] = json!("OpenAiCompatible");
    first.provenance_json["provider_endpoint"] = json!("https://example.test");
    first.provenance_json["provider_route_path_sha256"] = json!("path-a");
    sink.capture(first.clone()).await.expect("first capture");

    let mut rebound = first;
    rebound.provenance_json["provider_route_path_sha256"] = json!("path-b");
    let error = sink.capture(rebound).await.unwrap_err();
    assert!(
        error.to_string().contains("integrity violation"),
        "{error:#}"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn re_delivering_a_capture_is_idempotent() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();
    let rendered = fixture.turn(
        0,
        json!({"messages": [{"role": "user", "content": capture_text(5, 5 * 1024)}]}),
    );
    sink.capture(rendered.clone()).await.expect("first capture");
    let blocks_after_first = block_rows(&access).await.len();

    sink.capture(rendered.clone())
        .await
        .expect("re-delivery is idempotent");
    assert_eq!(
        block_rows(&access).await.len(),
        blocks_after_first,
        "no block may be written twice"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn concurrent_identical_captures_store_each_block_once() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();

    let request_json = json!({
        "model": "m",
        "messages": [{"role": "user", "content": capture_text(23, 256 * 1024)}]
    });
    let turns = (0..4)
        .map(|turn| fixture.turn(turn, request_json.clone()))
        .collect::<Vec<_>>();
    let results =
        futures::future::join_all(turns.iter().cloned().map(|rendered| sink.capture(rendered)))
            .await;
    for result in results {
        result.expect("every concurrent capture succeeds");
    }

    let rows = block_rows(&access).await;
    let keys = rows
        .iter()
        .map(|row| row["content_key"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(rows.len(), keys.len(), "one document per content key");
    let layout = super::super::encoding::capture_block_layout(&request_json).unwrap();
    let chunks = super::super::encoding::chunk_capture_layout(&layout);
    assert!(chunks.len() > CAPTURE_BLOCK_CREATE_BATCH);
    for chunk in chunks {
        assert!(keys.contains(&chunk.content_key));
    }
    for rendered in &turns {
        let row = stored_capture(&access, &rendered.capture_key).await;
        assert_eq!(
            super::super::decode_capture_json(
                &access,
                super::super::CAPTURE_VERSION,
                row["request_json"].as_str().unwrap(),
                super::super::CapturePayloadKind::RequestBody,
            )
            .await
            .unwrap(),
            super::super::canonical_json(&request_json)
        );
    }
    node.shutdown().await.expect("node shutdown");
}

async fn block_payload_bytes(
    access: &crate::ConfigAccess,
) -> std::collections::BTreeMap<String, usize> {
    let response = access
        .execute(r#"{ RenderedRequestBlock { content_key payload } }"#)
        .await
        .unwrap();
    response["data"]["RenderedRequestBlock"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            (
                row["content_key"].as_str().unwrap().to_owned(),
                row["payload"].as_str().unwrap().len(),
            )
        })
        .collect()
}

/// The shape an agent loop captures each turn: stable tools, a growing
/// wire message list under an output budget that shrinks every call, and
/// a provenance trace whose context accounting changes on every call while
/// its tool-result list only grows. From `RETAINED_FROM` the trace also
/// retains the native message list, as it does once context is reduced.
const RETAINED_FROM: usize = 40;

fn agent_loop_turn(fixture: &CaptureFixture, turn: usize) -> RenderedCompletionRequest {
    let mut messages = vec![json!({"role": "user", "content": capture_text(1, 8 * 1024)})];
    let mut native =
        vec![json!({"role": "user", "content": [{"text": capture_text(1, 8 * 1024)}]})];
    let mut threaded = Vec::new();
    for step in 0..turn {
        let call_id = format!("call-{step:04}");
        messages.push(
            json!({"role": "assistant", "tool_calls": [{"id": call_id, "type": "function",
            "function": {"name": "exec", "arguments": "{\"cmd\":\"rg capture\"}"}}]}),
        );
        messages.push(json!({"role": "tool", "tool_call_id": call_id,
            "content": capture_text(500 + step as u64, 3 * 1024)}));
        native.push(json!({"role": "assistant", "id": format!("msg-{step:04}")}));
        native.push(json!({"role": "user", "content": [{"tool_result": call_id,
            "text": capture_text(500 + step as u64, 3 * 1024)}]}));
        threaded.push(json!({"call_id": null, "message_index": 2 * step + 2, "tool_call_id": call_id,
            "content": [{"type": "text", "text": format!("[tool: exec, call_id: {call_id}, 3072 bytes — full output: sessions {}]", capture_text(900 + step as u64, 160))}]}));
    }
    let mut rendered = fixture.turn(
        turn,
        json!({"model": "m", "stream": true, "max_tokens": 32_000 - 300 * turn, "messages": messages,
            "tools": [{"type": "function", "function": {"name": "exec", "description": capture_text(2, 12 * 1024)}}]}),
    );
    rendered.provenance_payload_json = json!({
        "build_path": "budgeted",
        "context_accounting": {"accounting_version": 2, "attempt": 0, "compaction_reason": "below_threshold",
            "compaction_threshold_tokens": 786_432, "context_window": 1_048_576,
            "configured_max_output_tokens": 32_768, "estimated_input_tokens": 6000 + 900 * turn,
            "components": {"additional_parameters": 82, "documents": 0, "messages": 5000 + 900 * turn,
                "output_schema": 0, "tool_schemas": 3000},
            "estimator": "openai_chat_wire_json_bytes_div_4_v1", "turn_index": turn},
        "effective_message_count": messages.len(),
        "reduction_keys": [],
        "trace_version": 4,
        "assistant_message_ids": [],
        "effective_messages": (turn >= RETAINED_FROM).then_some(native),
        "threaded_tool_results": threaded,
    });
    rendered
}

#[tokio::test]
async fn a_repeated_agent_loop_request_stores_only_its_appended_turn() {
    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let access = fixture.access();
    let sink = fixture.sink();
    let mut stored = std::collections::BTreeMap::new();
    let mut first_provenance_leaf = None;
    let mut checked_turns = 0;
    let mut previous_wire = std::collections::BTreeSet::new();
    for turn in 0..64 {
        let rendered = agent_loop_turn(&fixture, turn);
        sink.capture(rendered.clone()).await.unwrap();
        let after = block_payload_bytes(&access).await;
        let new_blocks = after
            .keys()
            .filter(|key| !stored.contains_key(*key))
            .count();
        let new_bytes: usize = after
            .iter()
            .filter(|(key, _)| !stored.contains_key(*key))
            .map(|(_, len)| len)
            .sum();
        stored = after;

        let row = stored_capture(&access, &rendered.capture_key).await;
        let wire = manifest_entries(
            &access,
            row["request_json"].as_str().unwrap(),
            "request_body",
        )
        .await
        .into_iter()
        .map(|(_, key, _)| key)
        .collect::<std::collections::BTreeSet<_>>();
        if turn > 0 {
            let fresh = wire.difference(&previous_wire).count();
            assert!(
                fresh <= 6,
                "turn {turn}: {fresh} fresh wire-body leaves; history was re-cut"
            );
        }
        previous_wire = wire;
        let leaves = manifest_entries(
            &access,
            row["request_json"].as_str().unwrap(),
            "provenance_payload",
        )
        .await;
        let first_leaf = (leaves.len() >= 3).then(|| leaves[0].1.clone());
        // Each turn appends ~3.6 KiB of wire content, repeated in the
        // native trace once it is retained; the remainder is the rewritten
        // tail leaves and right manifest spines of both payloads.
        // The turn that starts retaining native messages stores that
        // list's framing once; every other turn pays for its append.
        if turn != RETAINED_FROM && first_leaf.is_some() && first_provenance_leaf.is_some() {
            checked_turns += 1;
            assert_eq!(
                first_provenance_leaf, first_leaf,
                "turn {turn}: per-call accounting re-cut the stable provenance prefix"
            );
            assert!(
                new_blocks <= 14,
                "turn {turn}: {new_blocks} new blocks for one appended tool round"
            );
            assert!(
                new_bytes <= 32 * 1024,
                "turn {turn}: {new_bytes} new block bytes for one appended tool round"
            );
        }
        first_provenance_leaf = first_leaf;
        assert_eq!(
            super::super::decode_capture_pair(
                &access,
                super::super::CAPTURE_VERSION,
                row["request_json"].as_str().unwrap(),
            )
            .await
            .unwrap(),
            (
                super::super::canonical_json(&rendered.request_json),
                super::super::canonical_json(&rendered.provenance_payload_json)
            )
        );
    }
    assert!(
        checked_turns >= 24,
        "the provenance trace must span several blocks"
    );
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn the_collection_names_are_valid_graphql_identifiers() {
    crate::store::graphql::validate_collection_identifier(RENDERED_REQUEST_COLLECTION)
        .expect("RenderedRequest must be a valid GraphQL identifier");
    assert_eq!(RENDERED_REQUEST_COLLECTION, "RenderedRequest");
    crate::store::graphql::validate_collection_identifier(RENDERED_REQUEST_BLOCK_COLLECTION)
        .expect("RenderedRequestBlock must be a valid GraphQL identifier");
    assert_eq!(RENDERED_REQUEST_BLOCK_COLLECTION, "RenderedRequestBlock");
}

#[tokio::test]
async fn title_capture_uses_parent_session_and_managed_agent_native_signatures() {
    use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
    use gents_protocol::request_admission::RequestPurpose;

    let fixture = CaptureFixture::new().await;
    let node = fixture.db.node.clone();
    let root = fixture.db.node_identity.as_ref();
    let agent = fixture.agent.clone();
    let signer = agent.managed_agent_signer().unwrap();
    let agent_access = fixture.access();
    let session_id = fixture.lifecycle.request().session_id.clone();
    let response = agent_access
        .execute("{ AgentSession { _docID } }")
        .await
        .unwrap();
    let session_doc = response["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let parent = fixture.lifecycle.request().clone();
    let auxiliary = crate::request::lifecycle::materialize::write_pending_auxiliary_request(
        &node,
        &parent,
        RequestPurpose::TitleAudit,
        parent.content.clone(),
        None,
        &signer,
    )
    .await
    .unwrap();
    let mut lifecycle =
        RequestLifecycle::new_with_node_did(node.clone(), auxiliary, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let auxiliary = lifecycle.request();
    let mut rendered = rendered_turn(
        0,
        json!({"model": "m", "messages": [{"role": "user", "content": capture_text(1, 9 * 1024)}]}),
    );
    rendered.node_did = root.did().into();
    rendered.agent_did = agent.did().into();
    rendered.requester_did = agent.did().into();
    rendered.session_id = session_id;
    rendered.request_doc_id = auxiliary.doc_id.clone();
    rendered.request_id = auxiliary.request_id.clone();
    rendered.request_commit_cid = lifecycle.request_commit_cid().unwrap().into();
    rendered.capture_scope = "title.1".into();
    rendered.capture_key = super::super::capture_key(
        &rendered.node_did,
        &rendered.session_id,
        &rendered.request_doc_id,
        &rendered.capture_scope,
        rendered.turn_index,
        rendered.attempt,
    )
    .unwrap();
    rendered.provenance_json =
        serde_json::to_value(super::super::ProvenanceManifest::captured_only(
            rendered.capture_scope.clone(),
            None,
            None,
            rendered.assembly_trace.clone(),
        ))
        .unwrap();
    let sink = DefraRenderedRequestSink {
        node: node.clone(),
        agent_signer: Some(signer),
    };
    sink.capture(rendered.clone()).await.unwrap();

    let response = agent_access.execute(&format!(r#"{{ RenderedRequest(filter: {{capture_key: {{_eq: "{}"}}}}) {{ _docID session_doc_id requester_did agent_did request_doc_id capture_scope request_json }} RenderedRequestBlock {{ _docID session_doc_id agent_did payload }} }}"#, escape_graphql_string(&rendered.capture_key))).await.unwrap();
    let data = &response["data"];
    let captures = data["RenderedRequest"].as_array().unwrap();
    assert_eq!(captures.len(), 1);
    let capture = &captures[0];
    assert_eq!(capture["session_doc_id"], session_doc);
    assert_eq!(capture["requester_did"], agent.did());
    assert_eq!(capture["agent_did"], agent.did());
    assert_eq!(capture["request_doc_id"], rendered.request_doc_id);
    assert_eq!(capture["capture_scope"], "title.1");
    let blocks = data["RenderedRequestBlock"].as_array().unwrap();
    assert!(blocks.len() > 1);
    let mut docs = vec![capture["_docID"].as_str().unwrap()];
    for block in blocks {
        assert_eq!(block["session_doc_id"], session_doc);
        assert_eq!(block["agent_did"], agent.did());
        assert!(block["payload"]
            .as_str()
            .is_some_and(|payload| !payload.is_empty()));
        docs.push(block["_docID"].as_str().unwrap());
    }
    for doc in docs {
        let response = agent_access.execute(&format!(r#"{{ _commits(docID: "{}") {{ cid height fieldName signature {{ identity type }} }} }}"#, escape_graphql_string(doc))).await.unwrap();
        let commits = crate::store::graphql::composite_commits_at(
            response["data"].get("_commits"),
            "_commits",
        )
        .unwrap();
        assert!(!commits.is_empty());
        for commit in commits {
            let (block, signature) = node
                .authorized_signed_block_bytes(&commit.cid, Some(agent.did()))
                .await
                .unwrap();
            let block = defra_core::block::Block::from_dag_cbor(&block).unwrap();
            let signature = defra_core::block::Signature::from_dag_cbor(&signature).unwrap();
            let actual =
                db::block::verify::verified_signature_signer_did(&block, &signature).unwrap();
            assert_eq!(actual, agent.did());
        }
    }
    let (request, _) = super::super::decode_capture_pair(
        &agent_access,
        gents_protocol::rendered_request::CAPTURE_VERSION,
        capture["request_json"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        request,
        super::super::canonical_json(&rendered.request_json)
    );
    node.shutdown().await.unwrap();
}
