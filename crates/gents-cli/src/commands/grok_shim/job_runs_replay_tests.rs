//! A synthetic run with the golden run's durable facts, projected through the
//! job cursor. `GENTS_GROK_GOLDEN_RECORDING` points the same checks at a
//! private recording; nothing from one is committed.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use gents_protocol::job::{JobBudget, JobChildState, JobPhaseState, JobState, JobView};
use serde_json::Value;

use super::tests::{adopted, at, child, harness, progress, tick, updates, view};

/// The recorded keys this projection emits; `pause_message` is the one
/// addition for a run that is not active or complete.
const PROJECTED_KEYS: [&str; 20] = [
    "active_agents",
    "agent_budget",
    "agents",
    "agents_remaining",
    "agents_reserved",
    "agents_used",
    "current_phase",
    "elapsed_ms",
    "foreground",
    "last_event",
    "last_event_detail",
    "last_event_timestamp",
    "name",
    "objective",
    "pause_message",
    "phases",
    "revision",
    "run_id",
    "sessionUpdate",
    "status",
];
/// Recorded, and deliberately never emitted in v1.
const OMITTED_KEYS: [&str; 2] = ["agent_usage_incomplete", "result_summary"];
const AGENT_KEYS: [&str; 6] = [
    "agent_id",
    "duration_ms",
    "label",
    "phase",
    "state",
    "tokens_used",
];
const STATUSES: [&str; 11] = [
    "active",
    "user_paused",
    "back_off_paused",
    "no_progress_paused",
    "infra_paused",
    "blocked",
    "budget_limited",
    "interrupted",
    "failed",
    "cancelled",
    "complete",
];
const PHASE_STATES: [&str; 3] = ["pending", "active", "done"];
const AGENT_STATES: [&str; 3] = ["active", "done", "failed"];
const PHASES: [&str; 6] = ["scope", "survey", "draft", "verify", "revise", "report"];
const SPAWNED_BY_STEP: [u32; 6] = [3, 6, 9, 12, 14, 15];

/// Step `n` has phase `n` active and the children spawned so far; the last
/// step is the completed run.
fn golden_run() -> Vec<JobView> {
    (0..=PHASES.len())
        .map(|step| {
            let done = step == PHASES.len();
            let spawned = SPAWNED_BY_STEP[step.min(PHASES.len() - 1)];
            let settled = step.checked_sub(1).map_or(0, |n| SPAWNED_BY_STEP[n]);
            let mut job = view(JobState::Running);
            let mut run = progress(
                PHASES
                    .iter()
                    .enumerate()
                    .map(|(index, title)| {
                        let state = match index.cmp(&step) {
                            Ordering::Less => JobPhaseState::Done,
                            Ordering::Equal => JobPhaseState::Active,
                            Ordering::Greater => JobPhaseState::Pending,
                        };
                        (*title, state)
                    })
                    .collect(),
            );
            run.budget = JobBudget {
                total: 128,
                spent: spawned,
                remaining: 128 - spawned,
            };
            run.spawned = spawned;
            run.revision = 2 * step as u64 + 1;
            run.last_event = format!("step {step}");
            job.progress = Some(run);
            job.children = (0..spawned)
                .map(|index| {
                    let state = if done || index < settled {
                        JobChildState::Completed
                    } else {
                        JobChildState::Live
                    };
                    child(&format!("child-{index}"), state)
                })
                .collect();
            if done {
                job.state = JobState::Completed;
                job.completed_at = Some(job.created_at + 60_000);
            }
            job
        })
        .collect()
}

fn revisions(frames: &[Value]) -> Vec<u64> {
    frames
        .iter()
        .map(|frame| frame["revision"].as_u64().unwrap())
        .collect()
}

fn assert_in<'a>(vocabulary: &[&str], value: &'a Value, what: &str) -> &'a str {
    let value = value
        .as_str()
        .unwrap_or_else(|| panic!("{what} is not a string"));
    assert!(
        vocabulary.contains(&value),
        "{what} `{value}` is not recorded"
    );
    value
}

/// Every `workflow_updated` frame stays within the recorded shape, names only
/// spawned subagents and has a strictly higher revision than the one before.
fn assert_recorded_shape(frames: &[Value], keys: &[&str], spawned: &BTreeSet<String>) {
    for frame in frames {
        for key in frame.as_object().unwrap().keys() {
            assert!(keys.contains(&key.as_str()), "key `{key}` is not recorded");
        }
        assert_in(&STATUSES, &frame["status"], "status");
        for phase in frame["phases"].as_array().unwrap() {
            assert_in(&PHASE_STATES, &phase["state"], "phase state");
        }
        let agents = frame["agents"].as_array().unwrap();
        for agent in agents {
            for key in agent.as_object().unwrap().keys() {
                assert!(AGENT_KEYS.contains(&key.as_str()), "agent key `{key}`");
            }
            assert_in(&AGENT_STATES, &agent["state"], "agent state");
            let id = agent["agent_id"].as_str().unwrap();
            assert!(spawned.contains(id), "agent `{id}` was never spawned");
        }
        assert_eq!(
            frame["active_agents"],
            agents
                .iter()
                .filter(|agent| agent["state"] == "active")
                .count()
        );
    }
    let revisions = revisions(frames);
    assert!(
        revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "revisions {revisions:?}"
    );
}

#[tokio::test]
async fn synthetic_golden_run_frames_stay_within_recorded_shape() {
    let (_directory, projections, buffer, sender) = harness().await;
    let run = golden_run();
    assert_eq!(run[0].progress.as_ref().unwrap().phases.len(), 6);
    assert_eq!(run.last().unwrap().children.len(), 15);
    let mut cursor = adopted(&run[0]);
    for (step, job) in run.iter().enumerate() {
        tick(
            &mut cursor,
            &sender,
            &projections,
            at(step as i64),
            Ok(job.clone()),
        )
        .await;
    }
    let frames = updates(&buffer).await;
    assert_eq!(frames.len(), run.len());
    assert_eq!(frames[0]["status"], "active");
    let last = frames.last().unwrap();
    assert_eq!(last["status"], "complete");
    assert_eq!(last["agent_budget"], 128);
    assert_eq!(last["agents_remaining"], 113);
    assert_eq!(last["agents"].as_array().unwrap().len(), 15);
    // The caused-session projection names a subagent by its child session id.
    let spawned = run
        .last()
        .unwrap()
        .children
        .iter()
        .map(|child| child.session_id.clone())
        .collect();
    assert_recorded_shape(&frames, &PROJECTED_KEYS, &spawned);
    for frame in &frames {
        for key in OMITTED_KEYS {
            assert!(frame.get(key).is_none(), "`{key}` is not emitted");
        }
    }
}

#[tokio::test]
async fn restarted_cursor_never_regresses_revision() {
    let (_directory, projections, buffer, sender) = harness().await;
    let run = golden_run();
    let mut first = adopted(&run[0]);
    for (step, job) in run[..4].iter().enumerate() {
        tick(
            &mut first,
            &sender,
            &projections,
            at(step as i64),
            Ok(job.clone()),
        )
        .await;
    }
    let delivered = revisions(&updates(&buffer).await);
    let mut restarted = adopted(&run[3]);
    for (offset, job) in run[3..5].iter().enumerate() {
        tick(
            &mut restarted,
            &sender,
            &projections,
            at(10 + offset as i64),
            Ok(job.clone()),
        )
        .await;
    }
    let all = revisions(&updates(&buffer).await);
    let after = &all[delivered.len()..];
    assert_eq!(after.len(), 2);
    assert!(
        after[0] >= *delivered.last().unwrap(),
        "{after:?} after {delivered:?}"
    );
    assert!(after[0] < after[1]);
}

#[test]
fn recorded_golden_run_matches_projection() {
    let Some(path) = std::env::var_os("GENTS_GROK_GOLDEN_RECORDING") else {
        eprintln!("skipped: GENTS_GROK_GOLDEN_RECORDING is unset");
        return;
    };
    let recording = std::fs::read_to_string(path).unwrap();
    let updates: Vec<Value> = recording
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let line: Value = serde_json::from_str(line).unwrap();
            line.pointer("/params/update")
                .or_else(|| line.get("update"))
                .cloned()
                .unwrap_or(line)
        })
        .collect();
    let kind = |update: &Value| {
        update["sessionUpdate"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let spawned = updates
        .iter()
        .filter(|update| kind(update) == "subagent_spawned")
        .map(|update| update["subagent_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        updates
            .iter()
            .filter(|update| kind(update).starts_with("subagent_"))
            .count(),
        30
    );
    let frames: Vec<Value> = updates
        .into_iter()
        .filter(|update| kind(update) == "workflow_updated")
        .collect();
    assert_eq!(frames.len(), 8);
    let recorded: Vec<&str> = PROJECTED_KEYS.into_iter().chain(OMITTED_KEYS).collect();
    assert_recorded_shape(&frames, &recorded, &spawned);
}
