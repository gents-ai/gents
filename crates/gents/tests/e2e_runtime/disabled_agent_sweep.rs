use std::time::Duration;

use crate::support::builder_runtime::submit_request;
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_request_terminal,
};
use crate::support::test_db;

async fn disable_agent(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agent_did: &str,
) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    access
        .transact_write("test.disabled_agent_sweep.disable", |txn| {
            Box::pin(async move {
                let (_, mut agent) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .expect("sibling Agent");
                agent["enabled"] = false.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents_store::collection::Collection::Agent,
                    add: agent.clone(),
                    update: agent,
                }])?;
                apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .expect("disable the sibling Agent as its User");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_sibling_agent_does_not_stop_the_runtime() {
    let target = live_target().await;
    let db = test_db("disabled-agent-sweep").await;
    let actors = bind_target(&db, &target).await;
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

    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    submit_request(
        &actors,
        "req-disabled-sibling",
        "session-disabled-sibling",
        "interactive",
        "Briefly confirm that you can answer this request.",
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&actors, "req-disabled-sibling", Duration::from_secs(120)).await,
        "completed"
    );
    submit_request(
        &actors,
        "req-disabled-sibling-next",
        "session-disabled-sibling",
        "interactive",
        "Briefly confirm that you can answer another request.",
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            &actors,
            "req-disabled-sibling-next",
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    runtime.shutdown().await;
}
