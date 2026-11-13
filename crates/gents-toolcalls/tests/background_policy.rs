use gents_inference::llm::message::{Message, UserContent};
use gents_inference::llm::{HookAction, ToolCallHookAction};
use gents_store::identity::NodeIdentity;
use serde_json::json;

use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::*;

use chrono::{SubsecRound, Utc};
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use gents_toolcalls::test_support::background_hook::{
    budget_hook_fixture, invoke, tool_row, BudgetHookFixture,
};
struct NamedPendingTool(String);

impl ToolDyn for NamedPendingTool {
    fn name(&self) -> String {
        self.0.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: self.0.clone(),
                description: "Held background work".into(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

struct NoisyTool(String);

impl ToolDyn for NoisyTool {
    fn name(&self) -> String {
        self.0.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: self.0.clone(),
                description: "Completes with long output".into(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok("x".repeat(100)) })
    }
}

#[tokio::test]
async fn configured_background_lifetimes_and_waits_follow_the_target() {
    let executions = BackgroundExecutionRegistry::default();
    let fixture = budget_hook_fixture(
        "configured-background-timeouts",
        BackgroundToolRegistry::default(),
        executions.clone(),
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let BudgetHookFixture {
        db,
        access,
        hook,
        request_id,
        session_id: session,
        ..
    } = fixture;
    let node = db.node.clone();
    let tools: gents_documents::Tools = serde_json::from_value(json!({
        "tools_id": "timed", "node_did": db.node_identity.did(),
        "host": {"bash": {"mode": "ReadOnly", "background_enabled": true,
            "background_timeout_secs": 60}},
        "remote": {"services": [{"mcp_service_id": "search", "style": "flat",
            "tool_names": ["query"], "background_tool_names": ["query"],
            "background_timeout_secs": 120, "wait_timeout_secs": 1}]}
    }))
    .unwrap();
    let remote_name = gents_tools::mcp::meta::flat_tool_name("search", "query");
    let config = gents_tools::surface::BackgroundToolConfig {
        allowlist: vec!["bash".into(), remote_name.clone()],
        timeouts: gents_tools::surface::ToolTimeouts::from_document(&tools).background,
    };
    let hook = hook
        .with_background_tool_registry(BackgroundToolRegistry::from_config(
            vec![
                Box::new(NamedPendingTool("bash".into())),
                Box::new(NamedPendingTool(remote_name.clone())),
            ],
            &config,
        ))
        .with_remote_tools(tools.remote.clone());
    hook.on_completion_call(&user_text_message("configured background"), &[])
        .await;

    let mut children = Vec::new();
    for (index, (tool, lifetime, selected)) in [
        ("bash", 60, None),
        (remote_name.as_str(), 120, Some(("search", "query"))),
    ]
    .into_iter()
    .enumerate()
    {
        let lifetime_span = chrono::Duration::seconds(lifetime);
        let earliest = Utc::now().trunc_subsecs(0) + lifetime_span;
        let args = json!({"tool_name": tool, "args": {}}).to_string();
        let receipt = invoke(&hook, &format!("timed-{index}"), "spawn_process", &args).await;
        let latest = Utc::now() + lifetime_span;
        assert_eq!(receipt["status"], "running", "{receipt}");
        assert_eq!(
            receipt["effective_timeout_secs"],
            json!(lifetime),
            "{receipt}"
        );
        let child = receipt["tool_call_id"].as_str().unwrap().to_owned();
        let row = tool_row(&access, &session, &child).await;
        let deadline = chrono::DateTime::parse_from_rfc3339(row["deadline_at"].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            (earliest..=latest).contains(&deadline),
            "{tool}: {deadline} outside {earliest}..={latest}"
        );
        assert_eq!(
            (
                row["selected_service_id"].as_str(),
                row["selected_tool_name"].as_str()
            ),
            selected.unzip(),
            "{row}"
        );
        children.push(child);
    }

    let started = std::time::Instant::now();
    let args = json!({"tool_call_id": children[1]}).to_string();
    let waited = invoke(&hook, "timed-wait", "wait_process", &args).await;
    assert_eq!(waited["status"], "running", "{waited}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "configured 1s wait took {:?}",
        started.elapsed()
    );

    let args = json!({"tool_name": "bash", "args": {"timeout_secs": 0}}).to_string();
    let receipt = invoke(&hook, "timed-zero", "spawn_process", &args).await;
    assert_eq!(receipt["status"], "running", "{receipt}");
    assert_eq!(receipt["effective_timeout_secs"], json!(1), "{receipt}");
    children.push(receipt["tool_call_id"].as_str().unwrap().to_owned());

    for (index, child) in children.iter().enumerate() {
        let args = json!({"tool_call_id": child}).to_string();
        let result = invoke(
            &hook,
            &format!("timed-cancel-{index}"),
            "cancel_process",
            &args,
        )
        .await;
        assert_eq!(result["status"], "cancelled");
        executions.wait_for_completion(child).await;
    }
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&hook, &request_id));
    node.shutdown().await.expect("shut down test node");
}
#[tokio::test]
async fn background_completion_summary_honors_the_configured_output_budget() {
    let executions = BackgroundExecutionRegistry::default();
    let registry = BackgroundToolRegistry::from_tools(
        vec![Box::new(NoisyTool("bash".into()))],
        &["bash".into()],
    );
    let fixture = budget_hook_fixture(
        "background-output-budget",
        registry,
        executions.clone(),
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let BudgetHookFixture {
        db,
        user,
        owner,
        agent,
        access,
        hook,
        request_id,
        session_id: session,
    } = fixture;
    let node = db.node.clone();
    let node_view = gents_store::config_client::ConfigAccess::local(node.clone())
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(&request_id),
        ))
        .await;
    assert!(node_view.is_err() || node_view.unwrap()["data"]["AgentRequest"] == json!([]));
    {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let tools = json!({"node_did": db.node_identity.did(), "tools_id": format!("{}:tools", agent.did()),
            "host": {"bash": {"mode": "ReadOnly", "background_enabled": true,
                "max_output_chars": 10}}});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Tools,
            add: tools.clone(),
            update: tools,
        }])
        .unwrap();
        owner
            .transact_write("test.output_budget", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }
    hook.on_completion_call(&user_text_message("budgeted background"), &[])
        .await;
    let receipt = invoke(
        &hook,
        "budgeted-spawn",
        "spawn_process",
        r#"{"tool_name":"bash","args":{}}"#,
    )
    .await;
    let child = receipt["tool_call_id"].as_str().unwrap().to_owned();
    executions.wait_for_completion(&child).await;
    let row = tool_row(&access, &session, &child).await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");

    let history = gents_session::session::load_history_with_access(
        &access,
        &session,
        db.node_identity.did(),
        Some(user.did()),
    )
    .await
    .unwrap();
    let marker = format!("<tool-completion tool_call_id=\"{child}\"");
    let notification = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            UserContent::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .find(|text| text.contains(&marker))
        .unwrap_or_else(|| panic!("completion notification missing: {history:?}"));
    assert!(
        notification.contains(&format!("<result>{}...</result>", "x".repeat(10))),
        "{notification}"
    );

    {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let tools = json!({"node_did": db.node_identity.did(), "tools_id": format!("{}:tools", agent.did()),
            "host": {"bash": {"mode": "ReadOnly", "background_enabled": true,
                "max_output_chars": 25}}});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Tools,
            add: tools.clone(),
            update: tools,
        }])
        .unwrap();
        owner
            .transact_write("test.output_budget", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }
    let tool_doc_id = row["_docID"].as_str().unwrap();
    gents_toolcalls::background::completion::append_background_tool_completion(
        &node,
        &access,
        &session,
        &request_id,
        tool_doc_id,
        "bash",
        "completed",
        &"x".repeat(100),
        None,
        gents_protocol::request_lifecycle::RequestHopCause::Continuation,
    )
    .await
    .expect("redrive after a budget change replays the published notice");
    let history = gents_session::session::load_history_with_access(
        &access,
        &session,
        db.node_identity.did(),
        Some(user.did()),
    )
    .await
    .unwrap();
    let notices = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content),
            _ => None,
        })
        .flatten()
        .filter(|content| matches!(content, UserContent::Text(text) if text.text.contains(&marker)))
        .count();
    assert_eq!(notices, 1, "redrive must not publish a second notice");

    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(&hook, &request_id));
    node.shutdown().await.expect("shut down test node");
}
#[tokio::test]
async fn call_tool_persists_concrete_dispatch_identity_without_rewriting_alias() {
    use gents_documents::{RemoteServiceTools, RemoteToolStyle, RemoteTools};

    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "selected-tool-dispatch",
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = fixture.db.node.clone();
    let session_id = fixture.session_id.clone();
    let hook = fixture.hook.with_remote_tools(Some(RemoteTools {
        services: vec![RemoteServiceTools {
            mcp_service_id: "metrics-prod".into(),
            tool_names: vec!["query_metrics".into()],
            style: RemoteToolStyle::Discovery,
            ..Default::default()
        }],
    }));
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Query metrics"), &[])
            .await,
        HookAction::Continue
    ));

    async fn selected_row(
        access: &gents_store::config_client::ConfigAccess,
        session_id: &str,
        tool_call_id: &str,
    ) -> serde_json::Value {
        let session = gents_store::graphql::escape_graphql_string(session_id);
        let call = gents_store::graphql::escape_graphql_string(tool_call_id);
        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_call_id: {{ _eq: "{call}" }} }}) {{ selected_service_id selected_tool_name }} }}"#
            ))
            .await
            .expect("selected agent can read its tool call");
        let rows = response["data"]["AgentToolCall"]
            .as_array()
            .expect("tool call rows");
        let [row] = rows.as_slice() else {
            panic!("expected one tool call, found {}", rows.len());
        };
        row.clone()
    }
    let access = hook.actor_access().expect("hook has a selected agent");

    let args =
        r#"{"service_id":"metrics-prod","tool_name":"query_metrics","arguments":{"window":"5m"}}"#;
    accept_hook_tool_call(&hook, "call-selected", "call_tool", args, None).await;
    assert!(matches!(
        hook.on_tool_call("call_tool", None, "call-selected", args)
            .await,
        ToolCallHookAction::Continue
    ));
    let selected = selected_row(&access, &session_id, "call-selected").await;
    assert_eq!(
        selected
            .get("selected_service_id")
            .and_then(serde_json::Value::as_str),
        Some("metrics-prod")
    );
    assert_eq!(
        selected
            .get("selected_tool_name")
            .and_then(serde_json::Value::as_str),
        Some("query_metrics")
    );

    accept_hook_tool_call(&hook, "call-native", "read_file", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("read_file", None, "call-native", "{}")
            .await,
        ToolCallHookAction::Continue
    ));
    let native = selected_row(&access, &session_id, "call-native").await;
    assert!(native
        .get("selected_service_id")
        .is_none_or(serde_json::Value::is_null));
    assert!(native
        .get("selected_tool_name")
        .is_none_or(serde_json::Value::is_null));

    node.shutdown().await.expect("shut down test node");
}
