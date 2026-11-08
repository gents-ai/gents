//! A disabled Agent stays watched but holds no baseline, so its sweeps cannot
//! read its own rows; that must not stop the runtime serving the enabled Agent.

use std::time::Duration;

use gents::{DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::request_lifecycle::RequestLifecycleState;

use crate::support::builder_runtime::{builder_actors, submit_request};
use crate::support::interrupt::{
    wait_for_request_terminal_state, wait_for_runtime_ready_or_exit, BootedAgent,
};
use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};
use crate::support::test_db;

const MODEL: &str = "disabled-sibling-model";
const BACKEND: &str = "disabled-sibling-backend";
const MARKER: &str = "disabled-sibling-marker";

async fn disable_agent(access: &gents::ConfigAccess, node_did: &str, agent_did: &str) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    access
        .transact_write("test.disabled_agent_sweep.disable", |txn| {
            Box::pin(async move {
                let (_, mut agent) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .expect("sibling Agent");
                agent["enabled"] = false.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::Agent,
                    add: agent.clone(),
                    update: agent,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("disable the sibling Agent as its User");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_sibling_agent_does_not_stop_the_runtime() {
    let backend =
        MockStreamingBackend::start(MODEL, vec![StreamScript::completes(MARKER, ["done"])])
            .unwrap();
    let db = test_db("disabled-agent-sweep").await;
    let actors = builder_actors(&db, BACKEND, backend.endpoint(), MODEL, 1).await;
    let (_, sibling) = crate::support::install_managed_agent(
        &db,
        &actors.fixture.access,
        &actors.fixture.user,
        "Disabled sibling",
    )
    .await;
    disable_agent(
        &actors.fixture.access,
        actors.node_did(),
        &sibling.agent_did,
    )
    .await;

    let runtime = Gents::from_default_agent_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect("build the document runtime");
    let node_did = actors.node_did().to_owned();
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut handle = tokio::spawn(runtime.run(shutdown_rx));
    wait_for_runtime_ready_or_exit(db.node.as_ref(), &node_did, &mut handle).await;

    let doc_id = submit_request(
        &actors,
        "req-disabled-sibling",
        "session-disabled-sibling",
        "interactive",
        &format!("please answer {MARKER}"),
    )
    .await;
    assert_eq!(
        wait_for_request_terminal_state(&actors, &doc_id, Duration::from_secs(60)).await,
        RequestLifecycleState::Completed
    );
    assert!(
        !handle.is_finished(),
        "the runtime must keep serving after sweeping a disabled Agent"
    );
    BootedAgent::new(shutdown, handle, node_did)
        .shutdown()
        .await;
}
