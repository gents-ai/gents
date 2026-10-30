use super::*;
use crate::inference::llm::tool::Tool;
use crate::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use std::sync::Arc;

async fn seed(
    node: &Arc<defra_node::EmbeddedNode>,
    agent: &str,
    requester: Option<&str>,
    id: &str,
    title: &str,
    closed: bool,
) {
    ConfigAccess::write_local(node, "test.sessions.discovery", &format!(r#"mutation {{create_AgentSession(input: {{node_did: {},requester_did: {},session_id: {},agent_id: "engineer",created_at: "2025-01-01T00:00:00Z",closed_at: {},title: {{text: {},source: "user"}},tags: ["release"]}}) {{_docID}}}}"#,quoted(agent),requester.map(quoted).unwrap_or_else(||"null".into()),quoted(id),if closed {quoted("2025-01-02T00:00:00Z")}else{"null".into()},quoted(title))).await.unwrap();
}

async fn call(tool: &SessionHistoryTool, params: Value) -> Value {
    serde_json::from_str(
        &Tool::call(tool, serde_json::from_value(params).unwrap())
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn sessions_reads_cross_agent_documents_only_when_acp_allows_the_caller() {
    let identities = tempfile::tempdir().unwrap();
    let owner = pin_fixed_signing_identity(identities.path());
    let reader = KeyIdentity::load_or_create(identities.path().join("reader.key"), None).unwrap();
    let denied = KeyIdentity::load_or_create(identities.path().join("denied.key"), None).unwrap();
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let policy = node
        .add_dac_policy(
            owner.did(),
            r#"
name: Sessions reader test
resources:
  - name: sessions
    relations:
      - name: reader
    permissions:
      - name: read
        expr: reader
      - name: update
      - name: delete
"#,
        )
        .await
        .unwrap();
    ConfigAccess::local(node.clone())
        .add_collection(&crate::store::schema::AGENT_SESSION_SCHEMA.replace(
            "type AgentSession ",
            &format!("type AgentSession @policy(id: \"{policy}\", resource: \"sessions\") "),
        ))
        .await
        .unwrap();
    let owner_did = owner.did().to_owned();
    let doc = ConfigAccess::transact_local_write(&node, Some(owner_did.parse().unwrap()), "test.sessions.acp.create", |txn| {
            let did = owner_did.clone();
            Box::pin(async move {
                let value = txn.execute(&format!("mutation {{add_AgentSession(input: {{node_did: {}, requester_did: \"different-requester\", session_id: \"shared\", agent_id: \"engineer\", created_at: \"2026-01-01T00:00:00Z\"}}) {{_docID}}}}", quoted(&did))).await?;
                Ok::<_, anyhow::Error>(value["data"]["add_AgentSession"][0]["_docID"].as_str().with_context(|| format!("ACP session fixture creation failed: {value}"))?.to_owned())
            })
        }).await.unwrap();
    node.add_dac_actor_relationship(owner.did(), "AgentSession", &doc, "reader", reader.did())
        .await
        .unwrap();
    let allowed_tool = SessionHistoryTool::new(node.clone(), reader.did());
    assert_eq!(
        call(&allowed_tool, json!({"action":"count"})).await["count"],
        1
    );
    let found = call(&allowed_tool, json!({"action":"get","session_id":"shared"})).await;
    assert_eq!(found["session"]["node_did"], owner.did());
    assert_eq!(found["session"]["requester_did"], "different-requester");
    let denied_tool = SessionHistoryTool::new(node.clone(), denied.did());
    assert_eq!(
        call(&denied_tool, json!({"action":"count"})).await["count"],
        0
    );
    assert!(
        call(&denied_tool, json!({"action":"list"})).await["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let error = Tool::call(
        &denied_tool,
        serde_json::from_value(json!({"action":"get","session_id":"shared"})).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("unavailable or ambiguous under ACP"));
    let error = Tool::call(
        &denied_tool,
        serde_json::from_value(json!({"action":"output","session_id":"shared","call_id":"any"}))
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("unavailable or ambiguous under ACP"));
    node.shutdown().await;
}

async fn call_error(tool: &SessionHistoryTool, params: Value) -> String {
    Tool::call(tool, serde_json::from_value(params).unwrap())
        .await
        .unwrap_err()
        .to_string()
}

/// Completes `lifecycle` with a 9 000-line output behind a bounded view
/// and returns the full output.
async fn complete_with_large_output(
    lifecycle: &mut crate::tools::call::lifecycle::ToolCallLifecycle,
) -> String {
    use gents_protocol::output::{PayloadPresentation, PresentationPart};

    let raw: String = (0..9_000)
        .map(|line| format!("{line:05} aé€😀\n"))
        .collect();
    let bounded = "[Showing first 10 of many bytes]";
    lifecycle
        .complete_raw_with_presentation(
            &raw,
            bounded,
            PayloadPresentation::Composed {
                parts: vec![PresentationPart::Literal {
                    text: bounded.to_owned(),
                }],
            },
        )
        .await
        .unwrap();
    raw
}

#[tokio::test]
async fn output_pages_a_background_calls_stored_output_past_its_receipt() {
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };

    let fixture = published_admission(PublishedAdmissionOptions {
        name: "sessions-output-background".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut lifecycle = fixture.tool;
    lifecycle
        .background_with_receipt("{\"status\":\"running\",\"handle\":\"h\"}")
        .await
        .unwrap();
    let session = lifecycle.session_id().to_owned();
    let tool = SessionHistoryTool::new(fixture.node.clone(), fixture.node_did.clone());
    // While the call runs, its only delivery is the receipt: not output.
    let error = call_error(
        &tool,
        json!({"action":"output","session_id":session,"call_id":"spawn-provider-call"}),
    )
    .await;
    assert!(error.contains("no delivered output yet"), "{error}");
    let raw = complete_with_large_output(&mut lifecycle).await;

    let mut params =
        json!({"action":"output","session_id":session,"call_id":"spawn-provider-call"});
    let mut recovered = String::new();
    loop {
        let text = Tool::call(&tool, serde_json::from_value(params.clone()).unwrap())
            .await
            .unwrap();
        let page: Value = serde_json::from_str(&text).unwrap();
        recovered.push_str(page["text"].as_str().unwrap());
        assert_eq!(page["tool_call_doc_id"], lifecycle.doc_id().unwrap());
        if page["complete"] == true {
            break;
        }
        params = page["next_call"].clone();
    }
    assert_eq!(recovered, raw);
    fixture.node.shutdown().await;
}

#[tokio::test]
async fn output_pages_the_full_stored_tool_output_behind_a_bounded_view() {
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };

    let fixture = published_admission(PublishedAdmissionOptions {
        name: "sessions-output".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut lifecycle = fixture.tool;
    let raw = complete_with_large_output(&mut lifecycle).await;
    assert!(raw.len() > 2 * 50 * 1024);
    let session = lifecycle.session_id().to_owned();
    let tool = SessionHistoryTool::new(fixture.node.clone(), fixture.node_did.clone());

    for key in ["spawn-provider-call", "spawn-native-tool"] {
        let mut params = json!({"action":"output","session_id":session,"call_id":key});
        let mut recovered = String::new();
        let mut pages = 0;
        loop {
            let text = Tool::call(&tool, serde_json::from_value(params.clone()).unwrap())
                .await
                .unwrap();
            assert!(
                text.starts_with("{\"text\":"),
                "answer first: {}",
                &text[..80]
            );
            let page: Value = serde_json::from_str(&text).unwrap();
            let chunk = page["text"].as_str().unwrap();
            assert!(chunk.len() <= OUTPUT_PAGE_BYTES);
            recovered.push_str(chunk);
            pages += 1;
            assert_eq!(page["tool_call_doc_id"], lifecycle.doc_id().unwrap());
            if page["complete"] == true {
                assert!(page.get("next_call").is_none());
                break;
            }
            params = page["next_call"].clone();
        }
        assert_eq!(recovered, raw);
        assert!(pages > 3);
    }

    let first = call(
        &tool,
        json!({"action":"output","session_id":session,"call_id":"spawn-provider-call"}),
    )
    .await;
    let mut stale = first["next_call"].clone();
    stale["expected_hash"] = json!("sha256:other-version");
    let error = call_error(&tool, stale).await;
    assert!(error.contains("output changed"), "{error}");
    assert!(error.contains("next call: sessions"), "{error}");
    let mut unguarded = first["next_call"].clone();
    unguarded.as_object_mut().unwrap().remove("expected_hash");
    let error = call_error(&tool, unguarded).await;
    assert!(error.contains("requires expected_hash"), "{error}");

    let error = call_error(
        &tool,
        json!({"action":"output","session_id":session,"call_id":"no-such-call"}),
    )
    .await;
    assert!(
        error.contains("no tool call with call_id no-such-call"),
        "{error}"
    );
    let next: Value =
        serde_json::from_str(error.split_once("next call: sessions ").unwrap().1).unwrap();
    assert_eq!(next, json!({"action":"transcript","session_id":session}));

    let error = call_error(
        &tool,
        json!({"action":"output","call_id":"spawn-provider-call"}),
    )
    .await;
    assert!(error.contains("outside a session"), "{error}");

    let foreign = KeyIdentity::load_or_create(fixture.path.join("foreign.key"), None).unwrap();
    seed(
        &fixture.node,
        foreign.did(),
        None,
        "foreign-output",
        "Other",
        false,
    )
    .await;
    let error = call_error(
        &tool,
        json!({"action":"output","session_id":"foreign-output","call_id":"spawn-provider-call"}),
    )
    .await;
    assert!(error.contains("no tool call"), "{error}");

    let help = call(&tool, json!({"action":"help","topic":"output"})).await;
    assert!(help["help"].as_str().unwrap().contains("next_call"));
    fixture.node.shutdown().await;
    std::fs::remove_dir_all(fixture.path).unwrap();
}

#[tokio::test]
async fn discovery_counts_and_pages_canonical_sessions_without_requests() {
    let identities = tempfile::tempdir().unwrap();
    let owner = pin_fixed_signing_identity(identities.path());
    let foreign = KeyIdentity::load_or_create(identities.path().join("foreign.key"), None).unwrap();
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    seed(&node, owner.did(), None, "atlas-a", "Atlas original", true).await;
    seed(&node, owner.did(), None, "atlas-b", "Atlas followup", true).await;
    seed(&node, owner.did(), None, "support", "Support", false).await;
    seed(
        &node,
        foreign.did(),
        None,
        "foreign-session",
        "Foreign private",
        true,
    )
    .await;
    seed(
        &node,
        owner.did(),
        Some(foreign.did()),
        "restricted",
        "Requester private",
        true,
    )
    .await;
    for start in (0..5001).step_by(500) {
        let rows = (start..(start+500).min(5001)).map(|index|format!("{{node_did: {},request_id: {},session_id: \"support\",purpose: \"normal\",created_at: \"2026-01-01T00:00:00Z\"}}",quoted(owner.did()),quoted(&format!("noise-{index}")))).collect::<Vec<_>>().join(",");
        ConfigAccess::write_local(
            &node,
            "test.sessions.scan_bound",
            &format!("mutation {{create_AgentRequest(input: [{rows}]) {{_docID}}}}"),
        )
        .await
        .unwrap();
    }
    let tool = SessionHistoryTool::new(node.clone(), owner.did());
    let filter = json!({"status":"closed","tag":"release","text":"atlas"});
    assert_eq!(
        call(&tool, json!({"action":"count","filter":filter})).await["count"],
        2
    );
    assert_eq!(call(&tool, json!({"action":"count"})).await["count"], 5);
    let first = call(&tool, json!({"action":"list","filter":filter,"limit":1})).await;
    assert_eq!(first["sessions"][0]["session_id"], "atlas-b");
    let second = call(
        &tool,
        json!({"action":"list","filter":filter,"limit":1,"cursor":first["next_cursor"]}),
    )
    .await;
    assert_eq!(second["sessions"][0]["session_id"], "atlas-a");
    assert!(second["next_cursor"].is_null());
    assert_eq!(
        call(&tool, json!({"action":"count","filter":{"text":"atlas"}})).await["count"],
        2
    );
    let recovery = Tool::call(
        &tool,
        serde_json::from_value(json!({"action":"get","session_id":"atlas-typo"})).unwrap(),
    )
    .await
    .unwrap_err();
    assert!(recovery.to_string().contains("next call: sessions"));
    let cross_requester = call(&tool, json!({"action":"get","session_id":"restricted"})).await;
    assert_eq!(cross_requester["session"]["requester_did"], foreign.did());
    let cross_agent = call(
        &tool,
        json!({"action":"get","session_id":"foreign-session"}),
    )
    .await;
    assert_eq!(cross_agent["session"]["node_did"], foreign.did());
    node.shutdown().await;
}

#[test]
fn search_recovery_preserves_literal_text_and_requires_a_query_when_none_was_supplied() {
    for text in ["quoted \"decision\"\nline", "日本語", ""] {
        let args =
            serde_json::from_value(json!({"action":"search","filter":{"text":text}})).unwrap();
        let error = missing_search_query(&args).to_string();
        let next: Value =
            serde_json::from_str(error.split_once("next call: sessions ").unwrap().1).unwrap();
        assert_eq!(
            next["query"],
            if text.is_empty() {
                "<text to find>"
            } else {
                text
            }
        );
        assert!(next.get("filter").is_none());
    }
}

#[tokio::test]
async fn transcript_search_reconstructs_canonical_text_and_pages_hits() {
    let identities = tempfile::tempdir().unwrap();
    let owner = pin_fixed_signing_identity(identities.path());
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    seed(&node, owner.did(), None, "work", "Investigation", false).await;
    for (sequence, text) in [
        (1, "The retry decision was retain state."),
        (2, "The retry budget was three calls."),
    ] {
        crate::session::import_history_observation(
            &node,
            "physical-request",
            "work",
            owner.did(),
            None,
            text,
            &format!("evidence-{sequence}"),
            sequence,
            None,
        )
        .await;
    }
    crate::session::import_history_observation(
        &node,
        "foreign-request",
        "foreign",
        PIN_FIXED_DID,
        Some("other-requester"),
        "The retry secret is protected.",
        "private-evidence",
        1,
        None,
    )
    .await;
    let tool = SessionHistoryTool::new(node.clone(), owner.did());
    let error = Tool::call(
        &tool,
        serde_json::from_value(
            json!({"action":"search","filter":{"text":"retry decision","tag":"release"},"limit":1}),
        )
        .unwrap(),
    )
    .await
    .unwrap_err();
    let error = error.to_string();
    let recovery: Value =
        serde_json::from_str(error.split_once("next call: sessions ").unwrap().1).unwrap();
    assert_eq!(recovery["query"], "retry decision");
    assert_eq!(recovery["filter"], json!({"tag":"release"}));
    assert_eq!(recovery["limit"], 1);
    let recovered = call(&tool, recovery).await;
    assert_eq!(recovered["hits"][0]["session_id"], "work");
    assert_eq!(recovered["hits"][0]["sequence"], 1);
    let definition = Tool::definition(&tool, String::new()).await;
    for combinator in ["oneOf", "allOf", "anyOf"] {
        assert!(
            definition.parameters.get(combinator).is_none(),
            "top-level {combinator} is rejected by Claude Messages"
        );
    }
    for missing in [
        json!({"action":"search"}),
        json!({"action":"get"}),
        json!({"action":"transcript"}),
        json!({"action":"output"}),
    ] {
        let error = Tool::call(&tool, serde_json::from_value(missing.clone()).unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("next call: sessions"), "{missing}: {error}");
    }
    let validator = jsonschema::validator_for(&definition.parameters).unwrap();
    for valid in [
        json!({}),
        json!({"action":"search","query":"retry"}),
        json!({"action":"get","session_id":"work"}),
        json!({"action":"transcript","session_id":"work"}),
        json!({"action":"output","call_id":"call-1"}),
        json!({"action":"output","tool_call_doc_id":"bae-1","offset_bytes":4,"expected_hash":"sha256:x"}),
    ] {
        assert!(validator.is_valid(&valid), "{valid}");
    }
    let help = call(&tool, json!({"action":"help","topic":"search"})).await;
    assert!(help["help"].as_str().unwrap().contains("literal"));
    assert!(!help["help"].as_str().unwrap().contains("compaction"));
    let first = call(&tool, json!({"action":"search","query":"retry","limit":1})).await;
    assert_eq!(first["hits"][0]["sequence"], 1);
    assert!(first["hits"][0]["message_doc_id"].is_string());
    assert!(first["hits"][0]["payload_references"][0]["close_doc_id"].is_string());
    let second = call(
        &tool,
        json!({"action":"search","query":"retry","limit":1,"cursor":first["next_cursor"]}),
    )
    .await;
    assert_eq!(second["hits"][0]["sequence"], 2);
    assert!(second["next_cursor"].is_null());
    let first = call(
        &tool,
        json!({"action":"transcript","session_id":"work","limit":1}),
    )
    .await;
    assert_eq!(
        first["messages"][0]["text"],
        "The retry decision was retain state."
    );
    let second = call(
        &tool,
        json!({"action":"transcript","session_id":"work","limit":1,"cursor":first["next_cursor"]}),
    )
    .await;
    assert_eq!(second["messages"][0]["sequence"], 2);
    assert!(second["next_cursor"].is_null());
    node.shutdown().await;
}
