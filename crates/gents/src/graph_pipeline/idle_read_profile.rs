use super::run_graph_run_reconciler;
use crate::test_support::read_observation::ReadCapture;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber;

fn publish_change(node: &defra_node::EmbeddedNode, collection: &str, document: &str) {
    node.event_bus()
        .publish(events::Message::update(events::Update::new(
            document.to_owned(),
            Default::default(),
            collection.to_owned(),
            Vec::<u8>::new(),
            false,
            false,
        )));
}

#[tokio::test]
async fn graph_run_wakes_on_graph_changes_after_a_durable_empty_scan() {
    let db = crate::support::test_db("graph-empty-wake").await;
    let collections = crate::collection_ids(&db.node, ["AgentRequest", "GraphRun"]).unwrap();
    let reads = ReadCapture::new("reconcile_owned_graph_runs");
    let cancel = CancellationToken::new();
    let _cancel_on_exit = cancel.clone().drop_guard();
    let task = tokio::spawn(
        run_graph_run_reconciler(
            db.node.clone(),
            db.node_identity.did().into(),
            cancel.clone(),
        )
        .with_subscriber(reads.dispatcher()),
    );
    tokio::time::timeout(Duration::from_secs(2), reads.wait_for(1))
        .await
        .unwrap();
    publish_change(&db.node, &collections[0], "unrelated-request-document");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        reads.count(),
        1,
        "an empty graph owner must ignore request-only changes"
    );
    publish_change(&db.node, &collections[1], "new-graph-document");
    tokio::time::timeout(Duration::from_secs(2), reads.wait_for(2))
        .await
        .expect("a graph invalidation must force a durable scan");
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
#[ignore = "six-second quiet and request-traffic samples; writes the requested profile artifact"]
async fn graph_run_idle_read_profile() {
    let output = std::env::var_os("GENTS_GRAPH_RUN_READ_PROFILE")
        .expect("set GENTS_GRAPH_RUN_READ_PROFILE to the measurement output path");
    let mut samples = Vec::new();
    for noisy in [false, true] {
        let db = crate::support::test_db(if noisy {
            "profile-noisy-graphs"
        } else {
            "profile-quiet-graphs"
        })
        .await;
        let collections = crate::collection_ids(&db.node, ["AgentRequest"]).unwrap();
        let reads = ReadCapture::new("reconcile_owned_graph_runs");
        let cancel = CancellationToken::new();
        let _cancel_on_exit = cancel.clone().drop_guard();
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(
            run_graph_run_reconciler(
                db.node.clone(),
                db.node_identity.did().into(),
                cancel.clone(),
            )
            .with_subscriber(reads.dispatcher()),
        );
        tokio::time::timeout(Duration::from_secs(2), reads.wait_for(1))
            .await
            .unwrap();
        let updates = async {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                if noisy {
                    interval.tick().await;
                    publish_change(&db.node, &collections[0], "unrelated-profile-request");
                } else {
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            () = tokio::time::sleep_until(started + Duration::from_secs(6)) => {},
            () = updates => unreachable!(),
        }
        samples.push(
            serde_json::json!({"unrelated_updates_per_second": if noisy { 10 } else { 0 },
            "elapsed_ms": started.elapsed().as_millis(), "graph_sweep_reads": reads.count()}),
        );
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    std::fs::write(output, serde_json::to_vec_pretty(&samples).unwrap()).unwrap();
}
