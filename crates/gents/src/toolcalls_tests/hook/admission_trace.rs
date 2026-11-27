use super::*;
use crate::test_support::tools::HeldTool;
use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_protocol::job::BackgroundLimit;
use gents_session::request::lifecycle::{
    renew_execution_lease_once_at_with_access, RenewalAttemptOutcome,
};
use gents_store::test_support::paused_transaction::{
    PausedTransaction,
    TransactionCheckpoint::{Begin, Commit},
};
use gents_store::test_support::STALL_CEILING;
use gents_toolcalls::test_support::background_hook::{
    admission_receipt_matches, budget_hook_fixture, tool_row,
};
use tokio::sync::Notify;

const START: &str = "tool_call.start_running_canonical";
const ADMIT: &str = "tool_call.admit_spawned_background";
const RECEIPT: &str = "tool_call.complete_delivery";
const REFUSAL: &str = "tool_call.fail_raw_with_presentation";
const RENEW: &str = "lifecycle.renew_execution_lease";

async fn replay(trace: Trace) {
    let n = trace.constants["N"].as_u64().unwrap() as usize;
    let limit = BackgroundLimit::resolve(trace.constants["Max"].as_i64());
    assert_eq!(trace.constants["Max"], limit.get());
    let ids: Vec<_> = (1..=n).map(|i| format!("admission-{i}")).collect();
    let names: Vec<_> = (1..=n).map(|i| format!("held_{i}")).collect();
    let release: Vec<_> = (0..n).map(|_| Arc::new(Notify::new())).collect();
    let entered: Vec<_> = (0..n).map(|_| Arc::new(Notify::new())).collect();
    let tools = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            Box::new(HeldTool::notified(
                name,
                "Work held until the trace completes its child",
                json!({"type":"object","properties":{}}),
                "completed",
                entered[i].clone(),
                release[i].clone(),
            )) as Box<dyn gents_loop::tool::ToolDyn>
        })
        .collect();
    let registry = BackgroundToolRegistry::from_config(
        tools,
        &gents_tools::surface::BackgroundToolConfig {
            allowlist: names.clone(),
            limit,
            ..Default::default()
        },
    );
    let executions = BackgroundExecutionRegistry::default().with_release_wait(STALL_CEILING);
    let fixture = budget_hook_fixture(
        "admission-trace",
        registry,
        executions.clone(),
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let hook = &fixture.hook;
    hook.on_completion_call(&user_text_message("admit background work"), &[])
        .await;
    let args: Vec<_> = names
        .iter()
        .map(|name| json!({"tool_name":name,"args":{}}))
        .collect();
    let calls = ids.iter().map(|id| (id.as_str(), None)).collect::<Vec<_>>();
    let message = Message::Assistant {
        request_id: None,
        id: Some("admission-turn".into()),
        content: ids
            .iter()
            .zip(&args)
            .map(|(id, args)| {
                AssistantContent::ToolCall(ToolCall {
                    raw_arguments: None,
                    id: id.clone(),
                    call_id: None,
                    function: ToolFunction {
                        name: "spawn_process".into(),
                        arguments: args.clone(),
                    },
                    signature: None,
                    additional_params: None,
                })
            })
            .collect(),
    };
    publish_and_adopt_tool_turns(hook, &calls, message).await;
    let doc = hook.active_request_doc_id().await.unwrap();
    let doc_filter = gents_store::graphql::escape_graphql_string(&doc);
    let lease_read = format!(
        r#"{{AgentRequest(filter:{{_docID:{{_eq:"{doc_filter}"}}}}){{execution_generation execution_lease_expires_at}}}}"#
    );
    let child_read = format!(
        r#"{{AgentToolCall(filter:{{request_doc_id:{{_eq:"{doc_filter}"}}, await_mode:{{_eq:"background"}}}}){{tool_call_id lifecycle_state spawned_by_tool_call_doc_id}}}}"#
    );
    let args: Vec<_> = args.iter().map(ToString::to_string).collect();
    let envelope = gents_loop::tool::ToolCallEnvelope::default();
    let mut actors = RapidHashMap::default();
    let mut renewal = None;
    let mut renewal_aborts = 0;
    let mut due_deadline = None;
    let mut start_conflicted = false;
    let mut child_finished = vec![false; n];
    let mut max_live = 0;
    for step in &trace.steps {
        let actor = step
            .actor
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .map(|i| i as usize - 1);
        match step.action.as_str() {
            "Begin" => {
                let i = actor.unwrap();
                let call = actors.entry(i).or_insert_with(|| {
                    PausedTransaction::named_attempts(
                        &[START, ADMIT, RECEIPT, REFUSAL],
                        hook.on_tool_call("spawn_process", None, &ids[i], &args[i], &envelope),
                    )
                });
                if call.checkpoint().is_none() {
                    call.next_checkpoint().await;
                }
                let phase = &step.before["phase"][i];
                let expected = match phase.as_str().unwrap() {
                    "start" => START,
                    "admit" => ADMIT,
                    "publish" => RECEIPT,
                    other => panic!("unknown admission phase {other}"),
                };
                match call.checkpoint() {
                    Some(Begin(REFUSAL)) if expected == ADMIT => {}
                    Some(Begin(name))
                        if name == expected || (name == REFUSAL && expected == RECEIPT) =>
                    {
                        let reached = call.next_checkpoint().await;
                        assert!(
                            reached == Some(Commit(name))
                                || (name == ADMIT && reached == Some(Begin(REFUSAL))),
                            "{} {name} reached {reached:?}",
                            trace.config
                        );
                    }
                    other => panic!(
                        "{} expected to begin {expected}, reached {other:?}",
                        trace.config
                    ),
                }
            }
            "CommitStart" | "CommitAdmission" | "CommitReceipt" | "Abort" => {
                let call = actors.get_mut(&actor.unwrap()).unwrap();
                match call.checkpoint() {
                    Some(Commit(name)) => {
                        let mut next = call.next_checkpoint().await;
                        while step.action != "Abort" && next == Some(Begin(name)) {
                            next = call.next_checkpoint().await;
                            if next == Some(Commit(name)) {
                                next = call.next_checkpoint().await;
                            }
                        }
                        if step.action == "Abort" && name == START && next == Some(Begin(START)) {
                            start_conflicted = true;
                        }
                    }
                    Some(Begin(REFUSAL))
                        if matches!(step.action.as_str(), "CommitAdmission" | "Abort") => {}
                    other => panic!("{} {} reached {other:?}", trace.config, step.action),
                }
                if step.action == "CommitAdmission" {
                    let data = fixture.access.execute(&child_read).await.unwrap();
                    let live = data["data"]["AgentToolCall"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| {
                            matches!(r["lifecycle_state"].as_str(), Some("pending" | "running"))
                        })
                        .count();
                    max_live = max_live.max(live);
                }
            }
            "ChildCompletes" => {
                let i = actor.unwrap();
                let parent = tool_row(&fixture.access, &fixture.session_id, &ids[i]).await;
                let child = format!("spawned:{}", parent["_docID"].as_str().unwrap());
                tokio::time::timeout(STALL_CEILING, entered[i].notified())
                    .await
                    .expect("child entered");
                release[i].notify_one();
                executions.wait_for_completion(&child).await;
                assert_eq!(
                    tool_row(&fixture.access, &fixture.session_id, &child).await["lifecycle_state"],
                    "completed"
                );
                child_finished[i] = true;
            }
            "RenewBegin" => {
                if renewal.is_none() {
                    let row = fixture.access.execute(&lease_read).await.unwrap()["data"]
                        ["AgentRequest"][0]
                        .clone();
                    let generation = row["execution_generation"].as_str().unwrap().to_owned();
                    let deadline = row["execution_lease_expires_at"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap();
                    due_deadline = Some(deadline);
                    let (access, doc) = (&fixture.access, &doc);
                    renewal = Some(PausedTransaction::named_attempts(&[RENEW], async move {
                        renew_execution_lease_once_at_with_access(
                            access,
                            doc,
                            &generation,
                            deadline,
                            deadline - chrono::TimeDelta::milliseconds(1),
                        )
                        .await
                        .unwrap()
                    }));
                    assert_eq!(
                        renewal.as_mut().unwrap().next_checkpoint().await,
                        Some(Begin(RENEW))
                    );
                }
                assert_eq!(
                    renewal.as_mut().unwrap().next_checkpoint().await,
                    Some(Commit(RENEW))
                );
            }
            "RenewAbort" => {
                assert_eq!(
                    renewal.as_mut().unwrap().next_checkpoint().await,
                    Some(Begin(RENEW))
                );
                renewal_aborts += 1;
            }
            "RenewCommit" => assert_eq!(
                renewal.take().unwrap().finish().await,
                RenewalAttemptOutcome::Committed
            ),
            action => panic!("unmapped admission step {action}"),
        }
    }
    if let Some(renewal) = renewal {
        assert_eq!(renewal.finish().await, RenewalAttemptOutcome::Committed);
    }
    let mut settled = true;
    for (i, call) in actors {
        let ToolCallHookAction::Skip { reason } = call.finish().await else {
            panic!(
                "{} did not return an admission receipt for {}",
                trace.config, ids[i]
            );
        };
        let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
        let parent = tool_row(&fixture.access, &fixture.session_id, &ids[i]).await;
        let data = fixture.access.execute(&child_read).await.unwrap();
        let calls = data["data"]["AgentToolCall"].as_array().unwrap();
        settled &= admission_receipt_matches(&parent, calls, &receipt, limit);
    }
    let data = fixture.access.execute(&lease_read).await.unwrap();
    let deadline = data["data"]["AgentRequest"][0]["execution_lease_expires_at"]
        .as_str()
        .unwrap()
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let mut properties = vec![
        ("BudgetBound", max_live <= limit.get()),
        ("NoConflictLoss", settled),
        (
            "LeaseLive",
            due_deadline.map_or_else(|| deadline > chrono::Utc::now(), |before| deadline > before),
        ),
    ];
    let red = match trace.config.as_str() {
        "MCBackgroundAdmissionContentionStartWitness" => {
            properties.push(("StartNeverConflicts", !start_conflicted));
            Red::Witness
        }
        "MCBackgroundAdmissionContentionRenewalWitness" => {
            properties.push((
                "RenewalNeverLost",
                renewal_aborts < trace.constants["RenewAttempts"].as_u64().unwrap(),
            ));
            Red::Witness
        }
        _ => Red::Defect,
    };
    trace.check(red, &properties);
    for (i, held) in release.into_iter().enumerate() {
        if !child_finished[i] {
            held.notify_one();
        }
    }
    let data = fixture.access.execute(&child_read).await.unwrap();
    for child in data["data"]["AgentToolCall"].as_array().unwrap() {
        executions
            .wait_for_completion(child["tool_call_id"].as_str().unwrap())
            .await;
    }
    hook_execution_fixtures()
        .lock()
        .await
        .remove(&hook_execution_fixture_key(hook, &fixture.request_id));
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn background_admission_contention_traces() {
    let attempts = gents_store::config_client::TRANSACT_CONFLICT_MAX_RETRIES + 1;
    for json in [
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContention.sample-1.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionLeaseLapse.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionRenewalBudget.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionRenewalWitness.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionShortBudget.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionStartWitness.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionUnfenced.json"),
        include_str!("../../../proofs/tla/generated/traces/MCBackgroundAdmissionContentionUnspaced.json"),
    ] {
        replay(Trace::load(json, "BackgroundAdmissionContention", &[("RenewAttempts", attempts), ("StartAttempts", attempts)])).await;
    }
}
