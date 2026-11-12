use std::sync::Arc;

use gents_inference::llm::message::UserContent;
use gents_loop::tool::ToolDyn;

use gents_inference::llm::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};

use gents_loop::loop_stream::*;

use gents_automation::test_support::loop_stream::repeated_tool_failure::*;
use gents_automation::test_support::loop_stream::tool_execution::*;
use gents_automation::test_support::loop_stream::*;

#[tokio::test]
async fn config_container_repairs_preserve_native_validation_and_the_invalid_budget() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook_with_identity().await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let owner = identity.did().to_owned();
    let binding = gents_inference::test_support::install_test_agent(&node, &owner, "general").await;
    let agent_did = binding.agent_did.clone();
    let grants = gents_tools::surface::SelfConfigToolConfig {
        enabled: true,
        agent_did: agent_did.clone(),
        categories: ["profile".to_string()].into_iter().collect(),
        preview: true,
        no_lockout: false,
        ..Default::default()
    };
    let tools = gents_tools::builtin::self_config::build_self_config_tools(
        std::sync::Arc::default(),
        node.clone(),
        owner,
        Some(identity),
        &grants,
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::default()),
        Arc::new(gents_automation::triggers::task_run::TaskRuns),
    );
    let mut calls = (0..10)
        .map(|index| {
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                format!("config-repair-{index}"),
                "config".into(),
                serde_json::json!({
                    "argv": ["profile", "preview"],
                    "set": serde_json::json!({"display_name": "{\"literal\":true}"}).to_string(),
                    "options": serde_json::json!({"agent": &agent_did}).to_string()
                }),
            ))
        })
        .collect::<Vec<_>>();
    calls.push(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        "config-invalid-after-repair".into(),
        "config".into(),
        serde_json::json!({
            "argv": ["profile", "preview"],
            "set": serde_json::json!({"display_name": []}).to_string(),
            "options": serde_json::json!({"agent": &agent_did}).to_string()
        }),
    )));
    calls.push(RawStreamingChoice::FinalResponse(()));
    let model = ScriptedModel::new_turns(vec![
        calls,
        vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let collected = collect_owned_scripted_stream(
        run_loop_stream(
            model,
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("preview profiles")),
            Vec::new(),
            Arc::new(tools),
            owned_config(4),
        ),
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.tool_results.len(), 11);
    let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
    assert_eq!(rows.len(), 11);
    for index in 0..10 {
        let row = rows
            .iter()
            .find(|row| row["tool_call_id"] == format!("config-repair-{index}"))
            .unwrap();
        assert_eq!(
            row["lifecycle_state"], "completed",
            "{}",
            collected.tool_results[index]
        );
    }
    let refused = rows
        .iter()
        .find(|row| row["tool_call_id"] == "config-invalid-after-repair")
        .unwrap();
    assert_eq!(refused["lifecycle_state"], "failed");
    assert!(
        collected.tool_results[10].contains("display_name"),
        "{}",
        collected.tool_results[10]
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn managed_terminal_tool_result_terminates_loop() {
    let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let prompt = Message::user("run the slow tool");

    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(FixedTool {
        name: "echo".to_string(),
        output: "unreachable".to_string(),
    })];
    let model = ScriptedModel::new_turns(vec![echo_tool_turn()]);
    let stream = run_loop_stream(
        model,
        Some(hook.clone()),
        TaggedMessage::unassociated(prompt),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );

    let collected = gents_tools::runtime_context::scope_request_tool_execution(
        Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        tokio_util::sync::CancellationToken::new(),
        Box::pin(collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )),
    )
    .await;

    let error = collected
        .error
        .as_deref()
        .expect("stream should end with a terminal error");
    assert!(
        error.contains("PromptCancelled"),
        "expected the typed PromptCancelled terminal; got {error}"
    );
    assert!(
        error.contains("tool call deadline exceeded"),
        "expected the precise managed-timeout reason; got {error}"
    );
}

#[tokio::test]
async fn tool_call_turn_executes_threads_result_and_completes() {
    let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let prompt = Message::user("use the echo tool");

    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(EchoTool {
        name: "echo".to_string(),
        output: "ECHOED".to_string(),
    })];

    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(prompt),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.tool_results, vec!["ECHOED".to_string()]);
    assert_eq!(collected.final_text.as_deref(), Some("done"));

    let dispatched = model.seen_histories().await;
    let fresh_result = dispatched[1]
        .iter()
        .find_map(|message| match message {
            Message::User { content } => content.iter().find_map(|item| match item {
                UserContent::ToolResult(result) => Some(result),
                _ => None,
            }),
            _ => None,
        })
        .expect("the next provider dispatch carries the fresh tool result");
    assert!(
        tool_result_text(&fresh_result.content[0]).contains("ECHOED"),
        "fresh result must reach the next dispatch intact below threshold"
    );

    let persisted = gents_session::session::load_history_with_access(
        &lifecycle.access().unwrap(),
        &lifecycle.request().session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .expect("load persisted session history");
    let persisted_view = gents_loop::loop_stream::provider_view_tagged(
        persisted
            .into_iter()
            .map(gents_loop::loop_stream::TaggedMessage::unassociated)
            .collect(),
    )
    .expect("project durable history for a later request");
    let durable_result = persisted_view
        .iter()
        .flat_map(|row| match &row.message {
            Message::User { content } => content.iter().collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .find_map(|item| match item {
            UserContent::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("persisted tool result remains represented in provider history");
    let durable_stub = tool_result_text(&durable_result.content[0]);
    assert!(durable_stub.contains("full output: sessions"));
    assert!(durable_stub.contains("\"action\":\"output\""));
    assert!(durable_stub.contains("\"call_id\":\"call-1\""));
    assert!(!durable_stub.contains("ECHOED"));

    let resp = agent_response(
        &lifecycle.access().unwrap(),
        "query { AgentToolCall { _docID tool_name lifecycle_state } }",
    )
    .await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall query failed: {:?}",
        resp.errors
    );
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().any(|row| {
            row.get("tool_name").and_then(|value| value.as_str()) == Some("echo")
                && row.get("lifecycle_state").and_then(|value| value.as_str()) == Some("completed")
        }),
        "expected a completed echo tool call; rows: {rows:?}"
    );
    let tool_doc_id = rows
        .iter()
        .find(|row| row["tool_name"] == "echo")
        .and_then(|row| row["_docID"].as_str())
        .unwrap();
    let output = agent_tool_output(&lifecycle, tool_doc_id).await;
    assert!(output.contains("ECHOED"));
}

fn rejected_notice(text: &str) -> String {
    if text.contains("__gents_tool_lifecycle__") {
        return "leaked internal marker".into();
    }
    let Some((_, rest)) = text.split_once("arguments were rejected (") else {
        return format!("no rejection notice: {text}");
    };
    match rest.split_once(')').map(|(kind, _)| kind) {
        Some("malformed") => "malformed",
        Some("truncated") => "truncated",
        Some("unknown field") => "unknown_field",
        Some("missing field") => "missing_field",
        Some("wrong type") => "wrong_type",
        Some("schema mismatch") => "schema",
        _ => "unrecognised",
    }
    .into()
}

#[tokio::test]
async fn refused_tool_arguments_reach_the_model_and_the_row_as_the_lean_model_records_them() {
    use gents_loop::tool::{Tool, ToolDefinition};

    struct StrictArgsTool;
    #[derive(Debug, thiserror::Error)]
    #[error("strict tool error")]
    struct StrictToolError;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictArgs {
        #[allow(dead_code)]
        report_type: String,
        #[allow(dead_code)]
        findings: Vec<String>,
    }
    impl Tool for StrictArgsTool {
        const NAME: &'static str = "post_status";
        type Error = StrictToolError;
        type Args = StrictArgs;
        type Output = String;
        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
            panic!("the tool must not run on unparseable arguments");
        }
    }

    for case in gents_lean_vocab::families::argument_failure_refusal::cases::<String, String>() {
        let arguments = match case.input.failure.as_str() {
            "syntax" => serde_json::Value::String("{\"report_type\":?}".into()),
            "unknown_field" => {
                serde_json::json!({"report_type": "steward", "findings": [], "extra": true})
            }
            "missing_field" => serde_json::json!({"report_type": "steward"}),
            "wrong_type" => serde_json::json!({"report_type": "steward", "findings": "none"}),
            other => panic!("no arguments for {other}"),
        };
        let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
        let model = ScriptedModel::new_turns(vec![
            vec![
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    "call-1".to_string(),
                    "post_status".to_string(),
                    arguments,
                )),
                RawStreamingChoice::FinalResponse(()),
            ],
            vec![
                RawStreamingChoice::Message("ok".to_string()),
                RawStreamingChoice::FinalResponse(()),
            ],
        ]);
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StrictArgsTool)];
        let stream = run_loop_stream(
            model,
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("post a status report")),
            Vec::new(),
            Arc::new(tools),
            owned_config(4),
        );
        let collected = collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )
        .await;
        let notice = collected.tool_results.join("\n");
        let rows = tool_call_rows_named(&lifecycle.access().unwrap(), "post_status").await;
        assert_eq!(rows.len(), 1, "{:?}: {rows:?}", case.input);
        case.check(
            gents_lean_vocab::families::argument_failure_refusal::RefusalOutput {
                notice: rejected_notice(&notice),
                names_field: notice.contains("missing field `findings`")
                    || notice.contains("unknown field `extra`"),
                lifecycle_state: rows[0]["lifecycle_state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                failure_class: rows[0]["tool_failure_class"].as_str().map(str::to_owned),
                ends_request: collected.error.is_some(),
            },
        );
    }
}

async fn tool_call_rows_named(
    reader: &gents_store::config_client::ConfigAccess,
    tool_name: &str,
) -> Vec<serde_json::Value> {
    tool_call_rows_with_names(reader)
        .await
        .into_iter()
        .filter(|row| row["tool_name"] == tool_name)
        .collect()
}

async fn tool_call_rows_with_names(
    reader: &gents_store::config_client::ConfigAccess,
) -> Vec<serde_json::Value> {
    let response = agent_response(
        reader,
        "query { AgentToolCall { tool_name lifecycle_state tool_failure_class } }",
    )
    .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn composed_children_outlast_the_guest_clock_and_fail_without_ending_the_parent() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let request = serde_json::json!({"tool_calls": {"requests": [
        {"id": "slow", "tool_name": "slow", "arguments": {}},
        {"id": "late", "tool_name": "late", "arguments": {}}]}});
    let (home, record) = gents_pack::plugin::test_fixtures::installed_plugin(
        &gents_pack::plugin::test_fixtures::asking_plugin_wat(
            "tool_calls",
            "tool_results",
            &request,
            false,
        ),
        None,
    );
    let plugin = gents_toolcalls::plugin::tool::PluginTool::resolve(
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
            Some(home.path().to_owned()),
        )),
        &gents_documents::PluginToolRef {
            tool_calls: true,
            plugin: "team/plugin".into(),
            digest: Some(record.digest.clone()),
            input_fields: Vec::new(),
            job: None,
        },
        None,
    )
    .unwrap();
    let tools: Vec<Box<dyn ToolDyn>> = vec![
        Box::new(plugin),
        Box::new(ComposedChild { name: "slow" }),
        Box::new(ComposedChild { name: "late" }),
    ];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(plugin_call_then_text_turns()),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("compose")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = gents_tools::runtime_context::scope_request_tool_execution(
        Some(chrono::Utc::now() + chrono::Duration::seconds(60)),
        tokio_util::sync::CancellationToken::new(),
        Box::pin(collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )),
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.final_text.as_deref(), Some("seen"));
    let output: serde_json::Value = serde_json::from_str(&collected.tool_results[0]).unwrap();
    assert_eq!(
        output["tool_results"]["slow"]["result"], "slow done",
        "{output}"
    );
    assert_eq!(
        output["tool_results"]["late"]["error"],
        "late timed out after 50ms"
    );
    let rows = agent_response(
        &lifecycle.access().unwrap(),
        "query { AgentToolCall { tool_name lifecycle_state } }",
    )
    .await;
    let rows = rows.data.unwrap()["AgentToolCall"].clone();
    for (name, state) in [
        ("plugin", "completed"),
        ("slow", "completed"),
        ("late", "failed"),
    ] {
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|row| row["tool_name"] == name && row["lifecycle_state"] == state),
            "{name} {state}: {rows}"
        );
    }
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_script_that_stops_mid_child_settles_the_child_and_the_request_finishes() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let node = db.node.clone();
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(StoppingParent), Box::new(Stall)];
    let stream = run_loop_stream(
        ScriptedModel::new_turns(plugin_call_then_text_turns()),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("compose")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = gents_tools::runtime_context::scope_request_tool_execution(
        Some(chrono::Utc::now() + chrono::Duration::seconds(60)),
        tokio_util::sync::CancellationToken::new(),
        Box::pin(collect_owned_scripted_stream(
            stream,
            &hook,
            &writer,
            &mut lifecycle,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        )),
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    assert_eq!(collected.final_text.as_deref(), Some("seen"));
    let rows = lifecycle
        .access()
        .unwrap()
        .execute("query { AgentToolCall { tool_name lifecycle_state cancel_cause } }")
        .await
        .unwrap();
    let rows = rows["data"]["AgentToolCall"].as_array().unwrap().clone();
    let state = |name: &str| {
        rows.iter()
            .find(|row| row["tool_name"] == name)
            .map(|row| row["lifecycle_state"].as_str().unwrap().to_owned())
    };
    assert_eq!(state("stall").as_deref(), Some("cancelled"), "{rows:?}");
    assert_eq!(state("plugin").as_deref(), Some("failed"), "{rows:?}");
    // With the child still running, normal completion is refused with ForegroundRunning.
    let selection = writer.terminal_output(&lifecycle.request().doc_id).await;
    lifecycle
        .terminalize_owned(
            gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
            selection,
            None,
        )
        .await
        .expect("the request completes once its script's child is settled");
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn parallel_native_reads_publish_distinct_bound_unchanged_receipts() {
    let (db, hook, writer, mut lifecycle) = owned_test_hook_with_identity().await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    assert_eq!(node.node_identity_did(), Some(identity.did()));
    assert_eq!(lifecycle.request().node_did, identity.did());
    let root = tempfile::tempdir().unwrap();
    tokio::fs::write(root.path().join("a.txt"), "alpha")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("b.txt"), "beta")
        .await
        .unwrap();
    let tools = gents_tools::native::ToolSet::builder()
        .read_root(root.path())
        .read_file()
        .build()
        .build_native_tools()
        .unwrap();
    let model = ScriptedModel::new_turns(vec![
        vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "read-a".into(),
                "read_file".into(),
                serde_json::json!({"path":"a.txt"}),
            )),
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "read-b".into(),
                "read_file".into(),
                serde_json::json!({"path":"b.txt"}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let stream = run_loop_stream(
        model,
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("read both")),
        Vec::new(),
        Arc::new(tools),
        owned_config(4),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    assert!(collected.error.is_none(), "{:?}", collected.error);
    let rows = lifecycle.access().unwrap().execute("{ AgentToolCall { _docID tool_name lifecycle_state } ToolFileInventory { tool_call_doc_id request_doc_id verified_unchanged } }").await.unwrap();
    let calls: Vec<_> = rows["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["tool_name"] == "read_file")
        .collect();
    let receipts = rows["data"]["ToolFileInventory"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call["lifecycle_state"], "completed");
        let matches: Vec<_> = receipts
            .iter()
            .filter(|receipt| receipt["tool_call_doc_id"] == call["_docID"])
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["request_doc_id"], lifecycle.request().doc_id);
        assert_eq!(matches[0]["verified_unchanged"], true);
    }
    node.shutdown().await.expect("node shutdown");
}
