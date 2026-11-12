use super::*;
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};

struct PendingTool;

impl ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "slow_tool".into(),
                description: "Held background work for admission accounting".into(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

async fn assert_background_rows(
    access: &gents_store::config_client::ConfigAccess,
    request: &str,
    count: usize,
    state: &str,
) {
    let response = access.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ request_id: {{_eq: "{}"}}, await_mode: {{_eq: "background"}} }}) {{ _docID lifecycle_state tool_name }} }}"#,
        gents_store::graphql::escape_graphql_string(request),
    )).await.unwrap();
    let data = &response["data"];
    let rows = data["AgentToolCall"].as_array().unwrap();
    assert_eq!(rows.len(), count);
    for row in rows {
        assert_eq!(row["lifecycle_state"], state, "{row}");
        assert_eq!(row["tool_name"], "slow_tool", "{row}");
    }
}

#[tokio::test]
async fn generated_background_budget_uses_accepted_dispatch() {
    let witness = gents_lean_vocab::lean_r6_background_theorem_witnesses()
        .iter()
        .find(|w| w.witness_kind == "admission_bound")
        .expect("modeled admission bound");
    let executions = BackgroundExecutionRegistry::default();
    let registry =
        BackgroundToolRegistry::from_tools(vec![Box::new(PendingTool)], &["slow_tool".into()]);
    let fixture = budget_hook_fixture(
        "background-budget",
        registry,
        executions.clone(),
        gents_automation::lifecycle_hooks::composition(),
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
    hook.on_completion_call(&user_text_message("fill background capacity"), &[])
        .await;
    let args = r#"{"tool_name":"slow_tool","args":{}}"#;
    let mut children = Vec::new();
    for index in 0..witness.numeric_bound {
        let receipt = invoke(&hook, &format!("budget-{index}"), "spawn_process", args).await;
        assert_eq!(receipt["status"], "running");
        assert_eq!(receipt["await_mode"], witness.kind_field("await_mode"));
        let child = receipt["tool_call_id"].as_str().unwrap().to_owned();
        let row = tool_row(&access, &session, &child).await;
        assert_eq!(row["await_mode"], witness.kind_field("await_mode"));
        assert_background_rows(&access, &request_id, index + 1, "running").await;
        children.push(child);
    }
    let denied = invoke(&hook, "budget-overflow", "spawn_process", args).await;
    assert_eq!(
        denied["code"],
        witness.kind_field("error_code_on_violation")
    );
    assert_eq!(
        denied["current_backgrounded"].as_u64(),
        Some(witness.numeric_bound as u64)
    );
    assert_eq!(
        denied["max_backgrounded"].as_u64(),
        Some(witness.numeric_bound as u64)
    );
    assert_background_rows(&access, &request_id, witness.numeric_bound, "running").await;
    for (index, child) in children.iter().enumerate() {
        let args = json!({"tool_call_id": child}).to_string();
        let result = invoke(&hook, &format!("cancel-{index}"), "cancel_process", &args).await;
        assert_eq!(result["status"], "cancelled");
        executions.wait_for_completion(child).await;
    }
    assert_background_rows(&access, &request_id, witness.numeric_bound, "cancelled").await;
    let batch = gents_lean_vocab::lean_r6_backgrounding_cases()
        .iter()
        .filter(|case| case.group == "budget_batch")
        .collect::<Vec<_>>();
    assert!(!batch.is_empty(), "modeled admission schedule");
    let ids = (0..batch.len())
        .map(|index| format!("concurrent-budget-{index}"))
        .collect::<Vec<_>>();
    let calls = ids.iter().map(|id| (id.as_str(), None)).collect::<Vec<_>>();
    let message = Message::Assistant {
        request_id: None,
        id: Some("concurrent-budget-turn".into()),
        content: ids
            .iter()
            .zip(&batch)
            .map(|(id, case)| {
                AssistantContent::ToolCall(ToolCall {
                    raw_arguments: None,
                    id: id.clone(),
                    call_id: None,
                    function: ToolFunction {
                        name: case.action.clone(),
                        arguments: serde_json::from_str(args).unwrap(),
                    },
                    signature: None,
                    additional_params: None,
                })
            })
            .collect(),
    };
    publish_and_adopt_tool_turns(&hook, &calls, message).await;
    // The owned loop admits a turn's calls one at a time: `spawn_process` is
    // not a read footprint, so it is dispatched alone, and even a read batch
    // (`run_batch`) awaits each admission. Concurrent sibling admissions are
    // outside the attempt bound (tla/BackgroundAdmissionContention).
    let results = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut results = Vec::with_capacity(ids.len());
        for (id, case) in ids.iter().zip(&batch) {
            results.push(hook.on_tool_call(&case.action, None, id, args).await);
        }
        results
    })
    .await
    .expect("ordered admissions must settle while their children remain running");
    let mut admitted = Vec::new();
    let mut rejected = 0;
    for result in results {
        let ToolCallHookAction::Skip { reason } = result else {
            panic!("accepted spawn must return a receipt: {result:?}");
        };
        let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
        if receipt["status"] == "running" {
            admitted.push(receipt["tool_call_id"].as_str().unwrap().to_owned());
        } else {
            assert_eq!(
                receipt["code"],
                witness.kind_field("error_code_on_violation")
            );
            rejected += 1;
        }
    }
    assert_eq!(
        admitted.len(),
        batch.iter().filter(|case| case.legal).count()
    );
    assert_eq!(rejected, batch.iter().filter(|case| !case.legal).count());
    let data = access.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ request_id: {{_eq: "{}"}}, await_mode: {{_eq: "background"}}, lifecycle_state: {{_eq: "running"}} }}) {{ _docID tool_call_id }} }}"#,
        gents_store::graphql::escape_graphql_string(&request_id),
    )).await.unwrap();
    assert_eq!(
        data["data"]["AgentToolCall"].as_array().unwrap().len(),
        admitted.len()
    );
    for (index, child) in admitted.iter().enumerate() {
        let args = json!({"tool_call_id": child}).to_string();
        let result = invoke(
            &hook,
            &format!("cancel-concurrent-{index}"),
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

use gents_toolcalls::test_support::background_hook::{
    budget_hook_fixture, invoke, tool_row, BudgetHookFixture,
};
