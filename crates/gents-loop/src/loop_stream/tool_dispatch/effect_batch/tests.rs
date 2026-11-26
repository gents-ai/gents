use super::*;
use crate::test_support::doubles::{
    EffectBatchEvents as Events, EffectBatchHook as Hook, EffectBatchProbe as Probe,
};
use crate::tool_call_lifecycle::runtime::ToolScopeEntry;
use crate::tool_effects::ToolEffectDispatcher;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn dispatcher(hook: Hook) -> (EffectDispatcher<Hook>, Events) {
    let events = hook.events.clone();
    let dispatcher = EffectDispatcher {
        tools: Arc::new(vec![Box::new(Probe(events.clone()))]),
        hook: Arc::new(hook),
        parent_internal_id: "parent".into(),
        fatal: Default::default(),
        composed: Default::default(),
    };
    (dispatcher, events)
}

fn parent(token: tokio_util::sync::CancellationToken) -> ToolCallEnvelope {
    ToolScopeEntry::new(None, token).call()
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

async fn run(dispatcher: &EffectDispatcher<Hook>, calls: Vec<ToolEffectCall>) -> Vec<EffectResult> {
    dispatcher.batch(&parent(Default::default()), calls).await
}

fn texts(results: Vec<EffectResult>) -> Vec<String> {
    results
        .into_iter()
        .map(|result| result.unwrap().model_facing_text().to_owned())
        .collect()
}

#[test]
fn effect_width_conforms_to_lean() {
    #[derive(serde::Deserialize, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum Footprint {
        Read,
        Barrier,
    }
    let (dispatcher, _) = dispatcher(Hook::default());
    gents_lean_vocab::families::tool_execution_parallel_script_effect_width::replay::<Footprint>(
        |batch| {
            let calls: Vec<_> = batch
                .calls
                .iter()
                .map(|call| ToolEffectCall {
                    ordinal: 1,
                    name: "probe".into(),
                    arguments:
                        json!({"id": call.key, "write": call.footprint == Footprint::Barrier})
                            .to_string(),
                })
                .collect();
            crate::tool_effects::parallel::effect_width(
                &calls,
                dispatcher.tools.as_slice(),
                &ToolCallEnvelope::default(),
            ) as u64
        },
    );
}

#[tokio::test(start_paused = true)]
async fn four_one_second_reads_share_one_second_and_keep_ordinals_and_positions() {
    let (dispatcher, events) = dispatcher(Hook::default());
    let start = tokio::time::Instant::now();
    let results = run(&dispatcher, calls(4)).await;
    assert_eq!(start.elapsed(), Duration::from_secs(1));
    assert_eq!(texts(results), ["1", "2", "3", "4"]);
    let events = events.lock().unwrap();
    let reserved: Vec<_> = events
        .iter()
        .filter(|event| event.starts_with("reserve:"))
        .cloned()
        .collect();
    assert_eq!(
        reserved,
        ["reserve:1", "reserve:2", "reserve:3", "reserve:4"]
    );
    let at = |event: &str| events.iter().position(|seen| seen == event).unwrap();
    assert!(at("approved:4") < at("start:1"));
}

#[tokio::test(start_paused = true)]
async fn a_fifth_read_waits_for_the_first_four() {
    let (dispatcher, events) = dispatcher(Hook::default());
    let start = tokio::time::Instant::now();
    let results = run(&dispatcher, calls(5)).await;
    assert_eq!(start.elapsed(), Duration::from_secs(2));
    assert_eq!(texts(results), ["1", "2", "3", "4", "5"]);
    let events = events.lock().unwrap();
    let at = |event: &str| events.iter().position(|seen| seen == event).unwrap();
    assert!(at("end:4") < at("reserve:5"));
}

#[tokio::test(start_paused = true)]
async fn writers_are_barriers_and_completion_does_not_reorder_results() {
    let (dispatcher, events) = dispatcher(Hook::default());
    let mut requests = calls(4);
    requests[0].arguments = json!({"id":1,"ms":20}).to_string();
    requests[1].arguments = json!({"id":2,"ms":1}).to_string();
    requests[2].arguments = json!({"id":3,"ms":1,"write":true}).to_string();
    assert_eq!(
        texts(run(&dispatcher, requests).await),
        ["1", "2", "3", "4"]
    );
    let events = events.lock().unwrap();
    let at = |event: &str| events.iter().position(|seen| seen == event).unwrap();
    assert!(at("end:2") < at("end:1"));
    assert!(at("end:1") < at("start:3"));
    assert!(at("end:3") < at("reserve:4"));
}

#[tokio::test(start_paused = true)]
async fn duplicate_calls_run_one_after_another() {
    let (dispatcher, events) = dispatcher(Hook::default());
    let mut requests = calls(2);
    requests[1].arguments = requests[0].arguments.clone();
    let start = tokio::time::Instant::now();
    assert_eq!(texts(run(&dispatcher, requests).await), ["1", "1"]);
    assert_eq!(start.elapsed(), Duration::from_secs(2));
    let events = events.lock().unwrap();
    assert_eq!(events.iter().filter(|event| *event == "start:1").count(), 2);
}

#[tokio::test(start_paused = true)]
async fn approvals_do_not_overlap_and_no_body_starts_before_group_approval() {
    let hook = Hook {
        block: true,
        ..Hook::default()
    };
    let (dispatcher, events) = dispatcher(hook);
    let parent = parent(Default::default());
    let work = dispatcher.batch(&parent, calls(2));
    tokio::pin!(work);
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut work)
        .await
        .is_err());
    assert_eq!(*events.lock().unwrap(), ["reserve:1", "ask:1"]);
    dispatcher.hook.approval.add_permits(1);
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut work)
        .await
        .is_err());
    assert_eq!(
        *events.lock().unwrap(),
        ["reserve:1", "ask:1", "approved:1", "reserve:2", "ask:2"]
    );
    dispatcher.hook.approval.add_permits(1);
    assert_eq!(work.await.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn terminating_admission_cancels_earlier_rows_without_starting_bodies() {
    let (dispatcher, events) = dispatcher(Hook {
        terminate: Some(2),
        ..Hook::default()
    });
    let errors: Vec<_> = run(&dispatcher, calls(4))
        .await
        .into_iter()
        .map(|result| match result {
            Err(ToolEffectError::Fatal(reason)) => reason,
            other => panic!("unexpected termination result: {other:?}"),
        })
        .collect();
    assert_eq!(errors, ["approval terminated", "approval terminated"]);
    let events = events.lock().unwrap();
    assert!(!events.iter().any(|event| event.starts_with("start:")));
    assert!(events.contains(&"finish:1:true".into()));
    assert!(events.contains(&"finish:2:true".into()));
    assert!(!events.contains(&"reserve:3".into()));
}

#[tokio::test(start_paused = true)]
async fn a_later_fatal_result_cancels_and_settles_earlier_read() {
    let (dispatcher, events) = dispatcher(Hook {
        terminate_result: Some(2),
        ..Hook::default()
    });
    let mut requests = calls(2);
    requests[1].arguments = json!({"id":2,"ms":1}).to_string();
    let results = run(&dispatcher, requests).await;
    assert!(matches!(&results[0], Ok(ToolOutcome::Cancelled)));
    assert!(matches!(&results[1], Err(ToolEffectError::Fatal(_))));
    let events = events.lock().unwrap();
    assert!(!events.contains(&"end:1".into()));
    assert!(events.contains(&"result:1".into()));
    assert!(events.contains(&"finish:1:false".into()));
}

#[tokio::test(start_paused = true)]
async fn denied_member_never_runs_but_other_approved_reads_do() {
    let (dispatcher, events) = dispatcher(Hook {
        skip: Some(2),
        ..Hook::default()
    });
    let results = run(&dispatcher, calls(3)).await;
    assert_eq!(results[1].as_ref().unwrap().model_facing_text(), "denied");
    let events = events.lock().unwrap();
    assert!(events.contains(&"start:1".into()));
    assert!(!events.contains(&"start:2".into()));
    assert!(events.contains(&"start:3".into()));
}

#[tokio::test(start_paused = true)]
async fn unavailable_reservation_preserves_reason_and_full_vector_at_every_position() {
    for ordinal in [1, 2, 5, 6] {
        let (dispatcher, events) = dispatcher(Hook {
            fail_reservation: Some(ordinal),
            ..Hook::default()
        });
        let results = run(&dispatcher, calls(6)).await;
        assert_eq!(results.len(), 6);
        let (group_start, group_end) = if ordinal <= 4 { (1, 4) } else { (5, 6) };
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
    let (dispatcher, events) = dispatcher(Hook::default());
    let token = tokio_util::sync::CancellationToken::new();
    let parent = parent(token.clone());
    let work = dispatcher.batch(&parent, calls(4));
    tokio::pin!(work);
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut work)
        .await
        .is_err());
    let started = events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.starts_with("start:"))
        .count();
    assert_eq!(started, 4);
    token.cancel();
    let results = work.await;
    assert_eq!(results.len(), 4);
    assert!(results
        .iter()
        .all(|result| matches!(result, Ok(ToolOutcome::Cancelled))));
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
    let (dispatcher, events) = dispatcher(Hook {
        fail_reservation: Some(1),
        ..Hook::default()
    });
    let mut requests = calls(2);
    for (index, call) in requests.iter_mut().enumerate() {
        call.arguments = json!({"id":index + 1,"write":true}).to_string();
    }
    let results = run(&dispatcher, requests).await;
    assert!(matches!(&results[0], Err(ToolEffectError::Unavailable(_))));
    assert_eq!(results[1].as_ref().unwrap().model_facing_text(), "2");
    assert!(events.lock().unwrap().contains(&"start:2".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn failed_read_group_does_not_block_following_writer() {
    let (dispatcher, events) = dispatcher(Hook {
        fail_reservation: Some(2),
        ..Hook::default()
    });
    let mut requests = calls(3);
    requests[2].arguments = json!({"id":3,"write":true}).to_string();
    let results = run(&dispatcher, requests).await;
    assert!(matches!(&results[0], Ok(ToolOutcome::Cancelled)));
    assert!(matches!(&results[1], Err(ToolEffectError::Unavailable(_))));
    assert_eq!(results[2].as_ref().unwrap().model_facing_text(), "3");
    assert!(events.lock().unwrap().contains(&"start:3".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn failed_cleanup_is_fatal_and_stops_later_groups() {
    let (dispatcher, _) = dispatcher(Hook {
        fail_reservation: Some(2),
        fail_finish: Some(1),
        ..Hook::default()
    });
    let mut requests = calls(3);
    requests[2].arguments = json!({"id":3,"write":true}).to_string();
    let results = run(&dispatcher, requests).await;
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

#[tokio::test]
async fn a_call_without_a_request_scope_is_unavailable() {
    let (dispatcher, events) = dispatcher(Hook::default());
    let results = dispatcher
        .batch(&ToolCallEnvelope::default(), calls(2))
        .await;
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|result| matches!(result, Err(ToolEffectError::Unavailable(_)))));
    assert!(events.lock().unwrap().is_empty());
}
