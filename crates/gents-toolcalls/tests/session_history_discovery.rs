use gents_loop::tool::Tool;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_tools::native::session_history::discovery::*;
use gents_tools::native::session_history::SessionHistoryTool;
use serde_json::{json, Value};
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
        &Tool::call(tool, serde_json::from_value(params).unwrap())
            .await
            .unwrap(),
    )
    .unwrap()
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
    lifecycle: &mut gents_toolcalls::call::lifecycle::ToolCallLifecycle,
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
    use gents_toolcalls::call::lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };

    let fixture = published_admission(PublishedAdmissionOptions {
        name: "sessions-output-background".into(),
        real_identity: true,
        composition: gents_session::test_support::no_lifecycle_plugins(),
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
    let tool = SessionHistoryTool::new(
        fixture.node.clone(),
        fixture.node_did.clone(),
        lifecycle.actor().clone(),
    );
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
    fixture.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn output_pages_the_full_stored_tool_output_behind_a_bounded_view() {
    use gents_toolcalls::call::lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };

    let fixture = published_admission(PublishedAdmissionOptions {
        name: "sessions-output".into(),
        real_identity: true,
        composition: gents_session::test_support::no_lifecycle_plugins(),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut lifecycle = fixture.tool;
    let raw = complete_with_large_output(&mut lifecycle).await;
    assert!(raw.len() > 2 * 50 * 1024);
    let session = lifecycle.session_id().to_owned();
    let tool = SessionHistoryTool::new(
        fixture.node.clone(),
        fixture.node_did.clone(),
        lifecycle.actor().clone(),
    );

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
    // The runtime signed the foreign session and grants the calling Agent no
    // read, so ACP hides it before any tool-call lookup.
    assert!(
        error.contains("unavailable or ambiguous under ACP"),
        "{error}"
    );

    let help = call(&tool, json!({"action":"help","topic":"output"})).await;
    assert!(help["help"].as_str().unwrap().contains("next_call"));
    fixture.node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(fixture.path).unwrap();
}
