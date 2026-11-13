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
    let cases = gents_lean_vocab::families::contracts_background_admission_bound::cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    let bound = usize::try_from(case.input.numeric_bound).unwrap();
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
    let mut admitted_modes = Vec::new();
    for index in 0..bound {
        let receipt = invoke(&hook, &format!("budget-{index}"), "spawn_process", args).await;
        assert_eq!(receipt["status"], "running");
        let child = receipt["tool_call_id"].as_str().unwrap().to_owned();
        let row = tool_row(&access, &session, &child).await;
        assert_eq!(row["await_mode"], receipt["await_mode"]);
        admitted_modes.push(receipt["await_mode"].as_str().unwrap().to_owned());
        assert_background_rows(&access, &request_id, index + 1, "running").await;
        children.push(child);
    }
    admitted_modes.dedup();
    assert_eq!(admitted_modes.len(), 1, "{admitted_modes:?}");
    let denied = invoke(&hook, "budget-overflow", "spawn_process", args).await;
    case.check(
        gents_lean_vocab::families::contracts_background_admission_bound::AdmissionObservation {
            kind_fields: [
                ("await_mode", Some(admitted_modes[0].as_str())),
                ("error_code_on_violation", denied["code"].as_str()),
            ]
            .into_iter()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    gents_lean_vocab::families::contracts_background_admission_bound::KindField {
                        key: key.to_owned(),
                        value: value.to_owned(),
                    }
                })
            })
            .collect(),
            current_backgrounded: denied["current_backgrounded"].as_u64().unwrap(),
            max_backgrounded: denied["max_backgrounded"].as_u64().unwrap(),
        },
    );
    assert_background_rows(&access, &request_id, bound, "running").await;
    for (index, child) in children.iter().enumerate() {
        let args = json!({"tool_call_id": child}).to_string();
        let result = invoke(&hook, &format!("cancel-{index}"), "cancel_process", &args).await;
        assert_eq!(result["status"], "cancelled");
        executions.wait_for_completion(child).await;
    }
    assert_background_rows(&access, &request_id, bound, "cancelled").await;
    let batch = gents_lean_vocab::families::contracts_r6_admission::cases();
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
                        name: case.input.action.clone(),
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
            results.push(hook.on_tool_call(&case.input.action, None, id, args).await);
        }
        results
    })
    .await
    .expect("ordered admissions must settle while their children remain running");
    let mut admitted = Vec::new();
    let mut attempts = Vec::new();
    for result in results {
        let ToolCallHookAction::Skip { reason } = result else {
            panic!("accepted spawn must return a receipt: {result:?}");
        };
        let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
        let live = admitted.len();
        let legal = receipt["status"] == "running";
        if legal {
            admitted.push(receipt["tool_call_id"].as_str().unwrap().to_owned());
        } else {
            assert_eq!(
                receipt["current_backgrounded"].as_u64(),
                Some(live as u64),
                "{receipt}"
            );
        }
        attempts.push((
            live as u64,
            gents_lean_vocab::families::contracts_r6_admission::AdmissionOutput {
                legal,
                terminal_state: if legal { "running" } else { "rejected" }.to_owned(),
                error_code: receipt["code"].as_str().map(str::to_owned),
            },
        ));
    }
    for case in &batch {
        let (live, observed) = attempts
            .iter()
            .find(|(live, _)| *live == case.input.pre_live_count)
            .unwrap_or_else(|| panic!("{}: no attempt saw this live count", case.input.name));
        assert!(*live <= case.input.max_backgrounded, "{}", case.input.name);
        case.check(observed.clone());
    }
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
