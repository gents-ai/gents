use super::support::*;
use crate::test_support::running_runtime::{AgentRuntimeHome, BootedRuntime};
use gents_inference::test_support::scripted_backend::{ScriptedBackend, StreamScript};
use gents_store::test_support::read_when_changed;
use gents_store::test_support::snapshot_reader::SnapshotReader as _;
use std::collections::BTreeSet;

const REQUEST: &str = "retire-held-request";

/// What the runtime shows of one retirement: the slot generations still
/// running (published and their task not joined) and the held request's live
/// calls (its request claimed or processing, or an inference call of it
/// holding a backend slot), while the call is held and once the retired slot
/// joins.
struct Retirement {
    retired_generation: u64,
    while_held: Option<(BTreeSet<u64>, u64)>,
    live_after: BTreeSet<u64>,
    live_calls_after: u64,
    finished: bool,
}

/// The held request's rows: its request as the User who submitted it reads
/// it, and its inference calls as the Agent that owns them reads them.
async fn live_calls(home: &AgentRuntimeHome) -> (u64, serde_json::Value) {
    let request = home
        .home
        .user_access
        .read_data(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{REQUEST}" }} }}) {{ lifecycle_state }} }}"#
        ))
        .await;
    let calls = home
        .agent_access()
        .read_data(&format!(
            r#"{{ InferenceCall(filter: {{ request_id: {{ _eq: "{REQUEST}" }} }}) {{ call_state }} }}"#
        ))
        .await;
    let data = serde_json::json!({
        "AgentRequest": request["AgentRequest"],
        "InferenceCall": calls["InferenceCall"],
    });
    let request_live = data["AgentRequest"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|row| {
            matches!(
                row["lifecycle_state"].as_str(),
                Some("claimed" | "processing")
            )
        });
    let backend_slot = data["InferenceCall"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|row| {
            gents_inference::admission::call_state_holds_backend_slot(
                row["call_state"].as_str().unwrap_or_default(),
            )
        });
    (u64::from(request_live || backend_slot), data)
}

/// Boots the runtime with its Agent's backend at capacity 1. With a call in
/// flight, the User submits a request that the generation-1 slot claims and
/// whose inference call the backend holds. Raising the backend's capacity
/// restages the Agent's slot: the supervisor publishes the next generation,
/// the router observes it, and the generation-1 slot is retired. The fixture
/// records what is running while the call is held, has the backend answer
/// it, and waits for the request to finish and the retired slot's task to
/// join.
async fn retire_runtime_slot(call_in_flight: bool) -> Retirement {
    let backend =
        ScriptedBackend::start("default", vec![StreamScript::completes(REQUEST, ["done"])])
            .unwrap();
    let held = backend.hold(REQUEST);
    let home = AgentRuntimeHome::start(
        if call_in_flight {
            "retire-slot-held"
        } else {
            "retire-slot-idle"
        },
        None,
        backend.endpoint(),
        1,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
    )
    .await;
    let agent_did = home.agent.did().to_owned();
    let mut runtime = BootedRuntime::boot(
        &home,
        crate::runtime::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await;
    let retired_generation = runtime
        .readiness()
        .await
        .expect("a ready runtime has readiness")
        .active_generation;

    if call_in_flight {
        home.submit_request(&agent_did, REQUEST, "retire-session", REQUEST)
            .await;
        held.entered().await;
        read_when_changed(
            &home.home.user_access,
            "InferenceCall",
            "the held call runs",
            async || match live_calls(&home).await {
                (1, data)
                    if data["InferenceCall"]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty()) =>
                {
                    Ok(())
                }
                (_, data) => Err(data.to_string()),
            },
        )
        .await;
    }

    home.bind_backend(backend.endpoint(), 2).await;
    let published = runtime
        .readiness_when("the restaged generation is active and routed", |snapshot| {
            snapshot.active_generation > retired_generation
                && snapshot.router_generation == snapshot.active_generation
        })
        .await
        .active_generation;
    let mut joined = BTreeSet::new();
    let record = |slots: Vec<(String, u64)>, joined: &mut BTreeSet<u64>| {
        for (agent, generation) in slots {
            assert_eq!(agent, agent_did, "only the restaged Agent's slot retires");
            joined.insert(generation);
        }
    };
    let running = |joined: &BTreeSet<u64>| -> BTreeSet<u64> {
        [retired_generation, published]
            .into_iter()
            .filter(|generation| !joined.contains(generation))
            .collect()
    };

    let while_held = if call_in_flight {
        record(runtime.joined_retired_slots(), &mut joined);
        let observed = (running(&joined), live_calls(&home).await.0);
        held.release();
        read_when_changed(
            &home.home.user_access,
            "AgentRequest",
            "the held request finishes",
            async || match live_calls(&home).await {
                (0, data) => Ok(data),
                (_, data) => Err(data.to_string()),
            },
        )
        .await;
        Some(observed)
    } else {
        None
    };
    while !joined.contains(&retired_generation) {
        let slot = runtime
            .next_joined_retired_slot(format!(
                "joined {joined:?}, published {published}, held request {}",
                live_calls(&home).await.1
            ))
            .await;
        record(vec![slot], &mut joined);
    }
    let (live_calls_after, after) = live_calls(&home).await;
    let retirement = Retirement {
        retired_generation,
        while_held,
        live_after: running(&joined),
        live_calls_after,
        finished: !call_in_flight
            || after["AgentRequest"][0]["lifecycle_state"].as_str() == Some("completed"),
    };
    runtime.shutdown().await.expect("the runtime stops cleanly");
    retirement
}

/// A retired slot keeps running its call in flight and its task joins once
/// that call finishes: the generation retires idle, does not retire while its
/// call is held, and retires after the call finishes.
#[tokio::test]
async fn retired_slot_exits_once_its_call_in_flight_finishes() {
    use gents_lean_vocab::families::contracts_runtime_reconcile_retire as family;
    for case in family::cases() {
        let retirement = retire_runtime_slot(case.input.tracked_request_id != 0).await;
        let live = if case.input.pre_in_flight_count == 0 {
            retirement.live_after
        } else {
            retirement.while_held.expect("a held call is observed").0
        };
        case.check(family::ReconcileRetireOutput {
            legal: !live.contains(&retirement.retired_generation),
            post_live_generation_count: live.len() as u64,
        });
    }
}

/// The request held on a retired slot finishes: its claim is finalized and
/// the backend slot its inference call held is released.
#[tokio::test]
async fn finished_call_on_a_retired_slot_releases_its_backend_slot() {
    use gents_lean_vocab::families::contracts_runtime_reconcile_finish as family;
    for case in family::cases() {
        let retirement = retire_runtime_slot(true).await;
        let (_, live_while_held) = retirement.while_held.expect("a held call is observed");
        assert_eq!(
            live_while_held, case.input.pre_in_flight_count,
            "the fixture holds the case's calls in flight"
        );
        case.check(family::ReconcileFinishOutput {
            legal: retirement.finished,
            post_in_flight_count: retirement.live_calls_after,
        });
    }
}
