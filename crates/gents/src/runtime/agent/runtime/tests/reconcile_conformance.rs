use super::support::*;
use crate::test_support::readiness::FailGenerationDefraWriter;
use crate::test_support::running_runtime::{AgentRuntimeHome, BootedRuntime};
use gents_inference::test_support::scripted_backend::{ScriptedBackend, Unscripted};
use gents_store::test_support::{read_when_changed, STALL_CEILING};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

async fn publish_changed_generation_on_real_runtime(pre_active_generation: u64) -> (u64, String) {
    let backend = ScriptedBackend::builder("default")
        .unscripted(Unscripted::Refuse)
        .start()
        .unwrap();
    let home = AgentRuntimeHome::start(
        "supervisor-publish",
        None,
        backend.endpoint(),
        1,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
    )
    .await;
    let node_did = home.home.node_did().to_owned();
    let runtime = BootedRuntime::boot(
        &home,
        crate::runtime::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await;
    let initial = runtime
        .readiness()
        .await
        .expect("a ready runtime publishes readiness");
    assert_eq!(initial.active_generation, pre_active_generation);

    home.bind_backend(backend.endpoint(), 2).await;
    let published = runtime
        .readiness_when("generation 2 publishes", |snapshot| {
            snapshot.active_generation > pre_active_generation
                && snapshot.router_generation == snapshot.active_generation
        })
        .await;
    let status = read_when_changed(
        home.home.node.as_ref(),
        "NodeRuntime",
        "reconcile settles to idle/applied",
        async || {
            let row = fetch_runtime_status(home.home.node.as_ref(), &node_did).await;
            if row.last_reconcile_result == "applied" && row.reconcile_phase == "idle" {
                Ok(row)
            } else {
                Err(format!("{row:?}"))
            }
        },
    )
    .await;
    assert!(status.last_reconcile_error.is_empty());
    runtime.shutdown().await.expect("the runtime stops cleanly");
    (published.active_generation, status.reconcile_phase)
}

async fn apply_failed_keeps_previous_generation_on_real_runtime(
    pre_active_generation: u64,
) -> (u64, String) {
    let backend = ScriptedBackend::builder("default")
        .unscripted(Unscripted::Refuse)
        .start()
        .unwrap();
    let home = AgentRuntimeHome::start(
        "supervisor-apply-failed",
        None,
        backend.endpoint(),
        1,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
    )
    .await;
    let node = home.home.store.node.clone();
    let identity = home.home.store.node_identity.clone();
    let node_did = identity.did().to_owned();
    let agent = crate::Gents::from_default_agent_documents(
        node.clone(),
        identity,
        crate::runtime::agent::DocumentRuntimeOptions {
            backend_admission: Some(home.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let writer = Arc::new(FailGenerationDefraWriter {
        node: node.clone(),
        fail_generation: pre_active_generation + 1,
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run = tokio::spawn(super::startup::run_agent_with_readiness_writer(
        agent,
        shutdown_rx,
        writer,
        Duration::from_secs(2),
    ));

    wait_for_runtime_process_state(node.as_ref(), &node_did, "ready").await;
    let initial = fetch_node_readiness(node.as_ref(), &node_did).await;
    assert_eq!(initial.active_generation, pre_active_generation);

    home.bind_backend(backend.endpoint(), 2).await;
    let failed_status = read_when_changed(
        node.as_ref(),
        "NodeRuntime",
        "failed apply settles to idle/error",
        async || {
            let row = fetch_runtime_status(node.as_ref(), &node_did).await;
            if row.last_reconcile_result == "error" && row.reconcile_phase == "idle" {
                Ok(row)
            } else {
                Err(format!("{row:?}"))
            }
        },
    )
    .await;
    assert!(!failed_status.last_reconcile_error.is_empty());
    let readiness = fetch_node_readiness(node.as_ref(), &node_did).await;

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(STALL_CEILING, run)
        .await
        .expect("runtime shuts down within ceiling")
        .expect("runtime task joins")
        .expect("runtime shuts down cleanly");
    (readiness.active_generation, failed_status.reconcile_phase)
}

/// The booted runtime's generation supervisor advances active_generation and
/// returns to idle when a configuration change applies, and keeps the previous
/// active_generation while returning to idle when readiness publication fails.
#[tokio::test]
async fn generation_supervisor_steps_match_lean_runtime_reconcile_cases() {
    use gents_lean_vocab::families::contracts_runtime_reconcile_supervisor as family;
    for case in family::cases() {
        let (post_active_generation, post_phase) = match case.input.action.as_str() {
            "publish" => {
                publish_changed_generation_on_real_runtime(case.input.pre_active_generation).await
            }
            "applyFailed" => {
                apply_failed_keeps_previous_generation_on_real_runtime(
                    case.input.pre_active_generation,
                )
                .await
            }
            other => panic!("the supervisor fixture cannot drive {other}"),
        };
        case.check(family::ReconcileSupervisorOutput {
            post_phase,
            post_active_generation,
        });
    }
}

/// The booted runtime's resolver never makes an Agent runnable when a tool
/// dependency it names is missing: the Agent is marked unavailable on the
/// published readiness snapshot while healthy Agents stay Ready.
#[tokio::test]
async fn resolver_marks_an_agent_with_a_missing_dependency_unavailable() {
    use gents_lean_vocab::families::contracts_runtime_reconcile_resolve as family;
    for case in family::cases() {
        let backend = ScriptedBackend::builder("default")
            .unscripted(Unscripted::Refuse)
            .start()
            .unwrap();
        let home = AgentRuntimeHome::start(
            "resolve-missing-dependency",
            None,
            backend.endpoint(),
            1,
            gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        )
        .await;
        let plugin_home = tempfile::tempdir().unwrap();
        let healthy_did = home.agent.did().to_owned();
        let runtime = BootedRuntime::boot(
            &home,
            crate::runtime::agent::DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::meta_only(),
                plugin_home: Some(plugin_home.path().to_path_buf()),
                ..Default::default()
            },
        )
        .await;
        let initial = runtime
            .readiness()
            .await
            .expect("a ready runtime publishes readiness");
        assert_eq!(initial.active_generation, case.input.pre_active_generation);

        let (missing_binding, _) = home
            .add_agent_with_tools(serde_json::json!({
                "integrations": {"plugins": [{"plugin": "acme/missing_plugin"}]}
            }))
            .await;
        let missing_did = missing_binding.agent_did;

        let snapshot = runtime
            .readiness_when(
                "missing-dependency agent appears in readiness",
                |readiness| {
                    readiness
                        .agents
                        .iter()
                        .any(|entry| entry.agent_did == missing_did)
                },
            )
            .await;
        assert!(
            snapshot.agents.iter().any(|entry| {
                entry.agent_did == healthy_did && entry.state == AgentReadinessState::Ready
            }),
            "healthy agent must remain ready"
        );
        let entry = snapshot
            .agents
            .iter()
            .find(|entry| entry.agent_did == missing_did)
            .expect("missing-dependency agent in readiness");
        assert_eq!(
            entry.reason,
            Some(AgentReadinessUnavailableReason::ToolSurfaceUnavailable)
        );
        runtime.shutdown().await.expect("the runtime stops cleanly");
        case.check(family::ReconcileResolveOutput {
            legal: entry.state == AgentReadinessState::Ready,
        });
    }
}
