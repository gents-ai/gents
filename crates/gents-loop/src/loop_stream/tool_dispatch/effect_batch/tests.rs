use super::*;
use crate::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError, ToolFootprint};
use crate::tool_call_lifecycle::runtime::scope_request_tool_execution;
use crate::tool_effects::ToolEffectDispatcher;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Events = Arc<Mutex<Vec<String>>>;

struct Probe(Events);
impl ToolDyn for Probe {
    fn hidden(&self) -> bool {
        false
    }
    fn name(&self) -> String {
        "probe".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "probe".into(),
                description: String::new(),
                parameters: json!({"type":"object"}),
            }
        })
    }
    fn footprint(&self, args: &str) -> ToolFootprint {
        if serde_json::from_str::<Value>(args).unwrap()["write"] == true {
            ToolFootprint::Barrier
        } else {
            ToolFootprint::Read
        }
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(&args).unwrap();
            let id = args["id"].as_u64().unwrap();
            self.0.lock().unwrap().push(format!("start:{id}"));
            tokio::time::sleep(Duration::from_millis(args["ms"].as_u64().unwrap_or(1000))).await;
            self.0.lock().unwrap().push(format!("end:{id}"));
            Ok(id.to_string())
        })
    }
}

struct Hook {
    events: Events,
    approval: tokio::sync::Semaphore,
    block: bool,
    terminate: Option<u32>,
    terminate_result: Option<u32>,
    skip: Option<u32>,
    fail_reservation: Option<u32>,
    fail_finish: Option<u32>,
}
#[async_trait::async_trait]
impl SessionHook for Hook {
    type Error = std::convert::Infallible;
    async fn admit_tool_effect(
        &self,
        _: &str,
        ordinal: u32,
        _: &str,
        _: &str,
    ) -> Result<String, String> {
        self.events
            .lock()
            .unwrap()
            .push(format!("reserve:{ordinal}"));
        if self.fail_reservation == Some(ordinal) {
            return Err(format!("reservation {ordinal} unavailable"));
        }
        Ok(ordinal.to_string())
    }
    async fn on_completion_call_with_context(
        &self,
        _: &gents_protocol::message::Message,
        _: &[gents_protocol::message::Message],
        _: Option<&gents_protocol::message::Message>,
    ) -> HookAction {
        HookAction::Continue
    }
    async fn on_tool_call(
        &self,
        _: &str,
        _: Option<String>,
        id: &str,
        _: &str,
    ) -> ToolCallHookAction {
        self.events.lock().unwrap().push(format!("ask:{id}"));
        if self.block {
            self.approval.acquire().await.unwrap().forget();
        }
        self.events.lock().unwrap().push(format!("approved:{id}"));
        let id: u32 = id.parse().unwrap();
        if self.terminate == Some(id) {
            ToolCallHookAction::Terminate {
                reason: "approval terminated".into(),
            }
        } else if self.skip == Some(id) {
            ToolCallHookAction::Skip {
                reason: "denied".into(),
            }
        } else {
            ToolCallHookAction::Continue
        }
    }
    async fn on_tool_admission_rejected(
        &self,
        _: &str,
        _: Option<String>,
        _: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
    }
    async fn on_tool_result(
        &self,
        _: &str,
        _: Option<String>,
        id: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> HookAction {
        self.events.lock().unwrap().push(format!("result:{id}"));
        if self.terminate_result == id.parse().ok() {
            HookAction::Terminate {
                reason: "settlement failed".into(),
            }
        } else {
            HookAction::Continue
        }
    }
    async fn finish_tool_effect(
        &self,
        id: &str,
        interruption: Option<&ToolOutcome>,
    ) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(format!("finish:{id}:{}", interruption.is_some()));
        if id.parse().ok() == self.fail_finish {
            return Err("finish failed".into());
        }
        Ok(())
    }
    async fn foreground_live_output_writer(
        &self,
        id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(id)
            .await
    }
    async fn session_id(&self) -> Option<String> {
        None
    }
    async fn register_stream_tool_call_identity(&self, _: &str, _: &str, _: Option<&str>) {}
}

fn setup() -> (EffectDispatcher<Hook>, Arc<Hook>, Events) {
    let events: Events = Default::default();
    let hook = Arc::new(Hook {
        events: events.clone(),
        approval: tokio::sync::Semaphore::new(0),
        block: false,
        terminate: None,
        terminate_result: None,
        skip: None,
        fail_reservation: None,
        fail_finish: None,
    });
    let dispatcher = EffectDispatcher {
        tools: Arc::new(vec![Box::new(Probe(events.clone()))]),
        hook: hook.clone(),
        parent_internal_id: "parent".into(),
        fatal: Default::default(),
        composed: Default::default(),
    };
    (dispatcher, hook, events)
}

fn calls(count: u32) -> Vec<ToolEffectCall> {
    (1..=count)
        .map(|ordinal| ToolEffectCall {
            ordinal,
            name: "probe".into(),
            arguments: json!({"id":ordinal,"ms":1000}).to_string(),
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn four_one_second_reads_share_one_second_and_keep_ordinals_and_positions() {
    let (dispatcher, _, events) = setup();
    let start = tokio::time::Instant::now();
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(calls(4))).await;
    assert_eq!(start.elapsed(), Duration::from_secs(1));
    let results: Vec<_> = results
        .into_iter()
        .map(|r| r.unwrap().model_facing_text().to_owned())
        .collect();
    assert_eq!(results, ["1", "2", "3", "4"]);
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.starts_with("reserve:"))
            .cloned()
            .collect::<Vec<_>>(),
        ["reserve:1", "reserve:2", "reserve:3", "reserve:4"]
    );
    assert!(
        events.iter().position(|e| e == "approved:4").unwrap()
            < events.iter().position(|e| e == "start:1").unwrap()
    );
}

#[tokio::test(start_paused = true)]
async fn writers_are_barriers_and_completion_does_not_reorder_results() {
    let (dispatcher, _, events) = setup();
    let mut requests = calls(4);
    requests[0].arguments = json!({"id":1,"ms":20}).to_string();
    requests[1].arguments = json!({"id":2,"ms":1}).to_string();
    requests[2].arguments = json!({"id":3,"ms":1,"write":true}).to_string();
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(requests)).await;
    assert_eq!(
        results
            .into_iter()
            .map(|r| r.unwrap().model_facing_text().to_owned())
            .collect::<Vec<_>>(),
        ["1", "2", "3", "4"]
    );
    let events = events.lock().unwrap();
    let at = |s| events.iter().position(|e| e == s).unwrap();
    assert!(at("end:2") < at("end:1"));
    assert!(at("end:1") < at("start:3"));
    assert!(at("end:3") < at("reserve:4"));
}

#[tokio::test(start_paused = true)]
async fn approvals_do_not_overlap_and_no_body_starts_before_group_approval() {
    let (mut dispatcher, mut hook, events) = setup();
    drop(dispatcher.hook);
    Arc::get_mut(&mut hook).unwrap().block = true;
    dispatcher.hook = hook.clone();
    let work = scope_request_tool_execution(None, Default::default(), dispatcher.batch(calls(2)));
    tokio::pin!(work);
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut work)
        .await
        .is_err());
    assert_eq!(*events.lock().unwrap(), ["reserve:1", "ask:1"]);
    hook.approval.add_permits(1);
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut work)
        .await
        .is_err());
    assert_eq!(
        *events.lock().unwrap(),
        ["reserve:1", "ask:1", "approved:1", "reserve:2", "ask:2"]
    );
    hook.approval.add_permits(1);
    assert_eq!(work.await.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn terminating_admission_cancels_earlier_rows_without_starting_bodies() {
    let (mut dispatcher, mut hook, events) = setup();
    drop(dispatcher.hook);
    Arc::get_mut(&mut hook).unwrap().terminate = Some(2);
    dispatcher.hook = hook;
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(calls(4))).await;
    let errors: Vec<_> = results
        .into_iter()
        .map(|result| match result {
            Err(ToolEffectError::Fatal(reason)) => reason,
            other => panic!("unexpected termination result: {other:?}"),
        })
        .collect();
    assert_eq!(errors, ["approval terminated", "approval terminated"]);
    let events = events.lock().unwrap();
    assert!(!events.iter().any(|e| e.starts_with("start:")));
    assert!(events.contains(&"finish:1:true".into()));
    assert!(events.contains(&"finish:2:true".into()));
    assert!(!events.contains(&"reserve:3".into()));
}

#[tokio::test(start_paused = true)]
async fn a_later_fatal_result_cancels_and_settles_earlier_read() {
    let (mut dispatcher, mut hook, events) = setup();
    drop(dispatcher.hook);
    Arc::get_mut(&mut hook).unwrap().terminate_result = Some(2);
    dispatcher.hook = hook;
    let mut requests = calls(2);
    requests[1].arguments = json!({"id":2,"ms":1}).to_string();
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(requests)).await;
    assert!(matches!(&results[0], Ok(ToolOutcome::Cancelled)));
    assert!(matches!(&results[1], Err(ToolEffectError::Fatal(_))));
    let events = events.lock().unwrap();
    assert!(!events.contains(&"end:1".into()));
    assert!(events.contains(&"result:1".into()));
    assert!(events.contains(&"finish:1:false".into()));
}

#[test]
fn script_effect_width_refines_the_existing_lean_read_prefix() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let (dispatcher, _, _) = setup();
    let mut inputs = Vec::new();
    let mut actual = Vec::new();
    for len in 0..=6 {
        for mask in 0..(1 << len) {
            let mut requests = calls(len);
            for (index, call) in requests.iter_mut().enumerate() {
                call.arguments =
                    json!({"id": index, "write": mask & (1 << index) != 0}).to_string();
            }
            for duplicate in [false, true] {
                let mut requests = requests.clone();
                if duplicate && requests.len() > 1 {
                    requests[1].arguments = requests[0].arguments.clone();
                }
                let mut identities = Vec::new();
                let modeled: Vec<_> = requests.iter().map(|call| {
                    let pair = (&call.name, &call.arguments);
                    let key = identities.iter().position(|id| *id == pair).unwrap_or_else(|| { identities.push(pair); identities.len() - 1 });
                    json!({"key": key, "read": crate::tool_effects::parallel::is_read(dispatcher.tools.as_slice(), &call.name, &call.arguments)})
                }).collect();
                inputs.push(json!({"limit":4,"calls":modeled}));
                actual.push(crate::tool_effects::parallel::effect_width(
                    &requests,
                    dispatcher.tools.as_slice(),
                ));
            }
        }
    }
    let dir = gents_lean_contract::proofs_dir().unwrap();
    gents_lean_contract::run_lake_build(&dir).unwrap();
    let mut lean = Command::new("lake")
        .args([
            "env",
            "lean",
            "--run",
            "Proofs/Conformance/ScriptEffectBatch.lean",
        ])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(lean.stdin.take().unwrap(), "{}", json!(inputs)).unwrap();
    let output = lean.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected: Vec<usize> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, expected);
}

#[tokio::test(start_paused = true)]
async fn denied_member_never_runs_but_other_approved_reads_do() {
    let (mut dispatcher, mut hook, events) = setup();
    drop(dispatcher.hook);
    Arc::get_mut(&mut hook).unwrap().skip = Some(2);
    dispatcher.hook = hook;
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(calls(3))).await;
    assert_eq!(results[1].as_ref().unwrap().model_facing_text(), "denied");
    let events = events.lock().unwrap();
    assert!(events.contains(&"start:1".into()));
    assert!(!events.contains(&"start:2".into()));
    assert!(events.contains(&"start:3".into()));
}

#[tokio::test]
#[ignore = "wall-clock timing sample; run alone to record scheduling overhead"]
async fn four_one_second_reads_wall_clock() {
    let (dispatcher, _, events) = setup();
    let start = std::time::Instant::now();
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(calls(4))).await;
    eprintln!("A2 four one-second read calls: {:?}", start.elapsed());
    assert!(results.iter().all(Result::is_ok));
    let events = events.lock().unwrap();
    let first_end = events.iter().position(|e| e.starts_with("end:")).unwrap();
    assert_eq!(
        events[..first_end]
            .iter()
            .filter(|e| e.starts_with("start:"))
            .count(),
        4
    );
}

#[tokio::test(start_paused = true)]
async fn unavailable_reservation_preserves_reason_and_full_vector_at_every_position() {
    for ordinal in [1, 2, 5, 6] {
        let (mut dispatcher, mut hook, events) = setup();
        drop(dispatcher.hook);
        Arc::get_mut(&mut hook).unwrap().fail_reservation = Some(ordinal);
        dispatcher.hook = hook;
        let results =
            scope_request_tool_execution(None, Default::default(), dispatcher.batch(calls(6)))
                .await;
        assert_eq!(results.len(), 6);
        let group_start = if ordinal <= 4 { 1 } else { 5 };
        let group_end = if ordinal <= 4 { 4 } else { 6 };
        for (index, result) in results.iter().enumerate() {
            let id = index as u32 + 1;
            if id < group_start || id > group_end {
                assert_eq!(result.as_ref().unwrap().model_facing_text(), id.to_string());
            } else if id < ordinal {
                assert!(matches!(result, Ok(ToolOutcome::Cancelled)));
            } else {
                assert!(
                    matches!(result, Err(ToolEffectError::Unavailable(reason)) if reason == &format!("reservation {ordinal} unavailable"))
                );
            }
        }
        assert!(dispatcher.fatal.lock().await.is_none());
        let events = events.lock().unwrap();
        assert!(!(group_start..=group_end).any(|i| events.contains(&format!("start:{i}"))));
        for earlier in group_start..ordinal {
            assert!(events.contains(&format!("finish:{earlier}:true")));
        }
        if ordinal < group_end {
            assert!(!events.contains(&format!("reserve:{}", ordinal + 1)));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancelling_request_mid_group_drains_all_started_children() {
    let (dispatcher, _, events) = setup();
    let token = tokio_util::sync::CancellationToken::new();
    let work = scope_request_tool_execution(None, token.clone(), dispatcher.batch(calls(4)));
    tokio::pin!(work);
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut work)
        .await
        .is_err());
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.starts_with("start:"))
            .count(),
        4
    );
    token.cancel();
    let results = work.await;
    assert_eq!(results.len(), 4);
    assert!(results
        .iter()
        .all(|r| matches!(r, Ok(ToolOutcome::Cancelled))));
    assert!(dispatcher.fatal.lock().await.is_none());
    let events = events.lock().unwrap();
    for ordinal in 1..=4 {
        assert!(events.contains(&format!("result:{ordinal}")));
        assert!(events.contains(&format!("finish:{ordinal}:false")));
        assert!(!events.contains(&format!("end:{ordinal}")));
    }
}

#[tokio::test(start_paused = true)]
async fn failed_writer_reservation_does_not_block_next_writer() {
    let (mut dispatcher, mut hook, events) = setup();
    drop(dispatcher.hook);
    Arc::get_mut(&mut hook).unwrap().fail_reservation = Some(1);
    dispatcher.hook = hook;
    let mut requests = calls(2);
    for (i, call) in requests.iter_mut().enumerate() {
        call.arguments = json!({"id":i+1,"write":true}).to_string();
    }
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(requests)).await;
    assert!(matches!(&results[0], Err(ToolEffectError::Unavailable(_))));
    assert_eq!(results[1].as_ref().unwrap().model_facing_text(), "2");
    assert!(events.lock().unwrap().contains(&"start:2".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn failed_read_group_does_not_block_following_writer() {
    let (mut dispatcher, mut hook, events) = setup();
    drop(dispatcher.hook);
    Arc::get_mut(&mut hook).unwrap().fail_reservation = Some(2);
    dispatcher.hook = hook;
    let mut requests = calls(3);
    requests[2].arguments = json!({"id":3,"write":true}).to_string();
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(requests)).await;
    assert!(matches!(&results[0], Ok(ToolOutcome::Cancelled)));
    assert!(matches!(&results[1], Err(ToolEffectError::Unavailable(_))));
    assert_eq!(results[2].as_ref().unwrap().model_facing_text(), "3");
    assert!(events.lock().unwrap().contains(&"start:3".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn failed_cleanup_is_fatal_and_stops_later_groups() {
    let (mut dispatcher, mut hook, _) = setup();
    drop(dispatcher.hook);
    let hook_mut = Arc::get_mut(&mut hook).unwrap();
    hook_mut.fail_reservation = Some(2);
    hook_mut.fail_finish = Some(1);
    dispatcher.hook = hook;
    let mut requests = calls(3);
    requests[2].arguments = json!({"id":3,"write":true}).to_string();
    let results =
        scope_request_tool_execution(None, Default::default(), dispatcher.batch(requests)).await;
    assert!(
        matches!(&results[0], Err(ToolEffectError::Fatal(reason)) if reason == "finish failed")
    );
    assert!(matches!(&results[1], Err(ToolEffectError::Unavailable(_))));
    assert_eq!(results.len(), 2);
    assert_eq!(
        dispatcher.fatal.lock().await.as_deref(),
        Some("finish failed")
    );
}
