use super::*;
use gents_loop::tool::Tool;
use gents_session::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use std::sync::Arc;

async fn seed(
    node: &Arc<defra_node::Node>,
    agent: &str,
    requester: Option<&str>,
    id: &str,
    title: &str,
    closed: bool,
) {
    ConfigAccess::write_local(node, "test.sessions.discovery", &format!(r#"mutation {{create_AgentSession(input: {{node_did: {},requester_did: {},session_id: {},agent_did: "engineer",created_at: "2025-01-01T00:00:00Z",closed_at: {},title: {{text: {},source: "user"}},tags: ["release"]}}) {{_docID}}}}"#,quoted(agent),requester.map(quoted).unwrap_or_else(||"null".into()),quoted(id),if closed {quoted("2025-01-02T00:00:00Z")}else{"null".into()},quoted(title))).await.unwrap();
}

async fn call(tool: &SessionHistoryTool, params: Value) -> Value {
    serde_json::from_str(
        &Tool::call(
            tool,
            serde_json::from_value(params).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::ambient(),
        )
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
    // The owner is the runtime: a document it creates is signed by its real
    // actor, and ACP alone decides who else may read it.
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(owner.did())
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned())),
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
        .add_collection(&gents_store::schema::AGENT_SESSION_SCHEMA.replace(
            &format!(
                "@policy(id: \"{}\", resource: \"sessions\")",
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
            ),
            &format!("@policy(id: \"{policy}\", resource: \"sessions\")"),
        ))
        .await
        .unwrap();
    let owner_did = owner.did().to_owned();
    let doc = ConfigAccess::transact_local_write(&node, Some(owner_did.parse().unwrap()), "test.sessions.acp.create", |txn| {
            let did = owner_did.clone();
            Box::pin(async move {
                let value = txn.execute(&format!("mutation {{add_AgentSession(input: {{node_did: {}, requester_did: \"different-requester\", session_id: \"shared\", agent_did: \"engineer\", created_at: \"2026-01-01T00:00:00Z\"}}) {{_docID}}}}", quoted(&did))).await?;
                Ok::<_, anyhow::Error>(value["data"]["add_AgentSession"][0]["_docID"].as_str().with_context(|| format!("ACP session fixture creation failed: {value}"))?.to_owned())
            })
        }).await.unwrap();
    node.add_dac_actor_relationship(owner.did(), "AgentSession", &doc, "reader", reader.did())
        .await
        .unwrap();
    let allowed_tool = SessionHistoryTool::new(
        node.clone(),
        owner.did(),
        identity::Did::new((reader.did()).to_string()).unwrap(),
    );
    assert_eq!(
        call(&allowed_tool, json!({"action":"count"})).await["count"],
        1
    );
    let found = call(&allowed_tool, json!({"action":"get","session_id":"shared"})).await;
    assert_eq!(found["session"]["node_did"], owner.did());
    assert_eq!(found["session"]["requester_did"], "different-requester");
    let denied_tool = SessionHistoryTool::new(
        node.clone(),
        owner.did(),
        identity::Did::new((denied.did()).to_string()).unwrap(),
    );
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
        &gents_loop::tool::ToolCallEnvelope::ambient(),
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
        &gents_loop::tool::ToolCallEnvelope::ambient(),
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("unavailable or ambiguous under ACP"));
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn discovery_counts_and_pages_canonical_sessions_without_requests() {
    let identities = tempfile::tempdir().unwrap();
    let owner = pin_fixed_signing_identity(identities.path());
    let foreign = KeyIdentity::load_or_create(identities.path().join("foreign.key"), None).unwrap();
    // The fixture writes as the runtime, so the caller is that runtime.
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(owner.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
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
    let tool = SessionHistoryTool::new(
        node.clone(),
        owner.did(),
        identity::Did::new((owner.did()).to_string()).unwrap(),
    );
    let filter = json!({"status":"closed","tag":"release","text":"atlas"});
    assert_eq!(
        call(&tool, json!({"action":"count","filter":filter})).await["count"],
        2
    );
    assert_eq!(call(&tool, json!({"action":"count"})).await["count"], 4);
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
        &gents_loop::tool::ToolCallEnvelope::ambient(),
    )
    .await
    .unwrap_err();
    assert!(recovery.to_string().contains("next call: sessions"));
    let cross_requester = call(&tool, json!({"action":"get","session_id":"restricted"})).await;
    assert_eq!(cross_requester["session"]["requester_did"], foreign.did());
    let other_node = Tool::call(
        &tool,
        serde_json::from_value(json!({"action":"get","session_id":"foreign-session"})).unwrap(),
        &gents_loop::tool::ToolCallEnvelope::ambient(),
    )
    .await
    .unwrap_err();
    assert!(other_node
        .to_string()
        .contains("unavailable or ambiguous under ACP"));
    node.shutdown().await.expect("shut down test node");
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
    // The fixture writes as the runtime, so the caller is that runtime.
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(owner.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    seed(&node, owner.did(), None, "work", "Investigation", false).await;
    for (sequence, text) in [
        (1, "The retry decision was retain state."),
        (2, "The retry budget was three calls."),
    ] {
        gents_session::session::import_history_observation(
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
    gents_session::session::import_history_observation(
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
    let tool = SessionHistoryTool::new(
        node.clone(),
        owner.did(),
        identity::Did::new((owner.did()).to_string()).unwrap(),
    );
    let error = Tool::call(
        &tool,
        serde_json::from_value(
            json!({"action":"search","filter":{"text":"retry decision","tag":"release"},"limit":1}),
        )
        .unwrap(),
        &gents_loop::tool::ToolCallEnvelope::ambient(),
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
        let error = Tool::call(
            &tool,
            serde_json::from_value(missing.clone()).unwrap(),
            &gents_loop::tool::ToolCallEnvelope::ambient(),
        )
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
        first["messages"][0],
        json!({"sequence":1,"role":first["messages"][0]["role"],"text":"The retry decision was retain state."}),
        "a whole message is its sequence, role and text"
    );
    let detailed = call(
        &tool,
        json!({"action":"transcript","session_id":"work","limit":1,"details":true}),
    )
    .await;
    assert!(detailed["messages"][0]["message_doc_id"].is_string());
    assert!(detailed["messages"][0]["payload_references"][0]["close_doc_id"].is_string());
    let summary = call(&tool, json!({"action":"get","session_id":"work"})).await;
    assert_eq!(summary["session"]["session_id"], "work");
    assert!(
        summary.get("details").is_none()
            && summary["session"]["provenance"]
                .get("creation_receipt")
                .is_none()
            && !summary.to_string().contains("null"),
        "{summary}"
    );
    assert!(
        summary.to_string().len() <= 320,
        "sessions get is {} chars: {summary}",
        summary.to_string().len()
    );
    let whole = call(
        &tool,
        json!({"action":"get","session_id":"work","details":true}),
    )
    .await;
    assert!(whole["details"].is_object(), "{whole}");
    assert!(
        whole["session"].as_object().unwrap().len()
            >= summary["session"].as_object().unwrap().len(),
        "{whole}"
    );
    let second = call(
        &tool,
        json!({"action":"transcript","session_id":"work","limit":1,"cursor":first["next_cursor"]}),
    )
    .await;
    assert_eq!(second["messages"][0]["sequence"], 2);
    assert!(second["next_cursor"].is_null());
    node.shutdown().await.expect("shut down test node");
}
