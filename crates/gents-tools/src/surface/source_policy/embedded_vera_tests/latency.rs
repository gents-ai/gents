use std::time::{Duration, Instant};

use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::{Household, PROVIDERS};

const GRANTS: usize = 50;

/// For `GRANTS` staged `reader` grants in turn on an idle home: how long each
/// commit took, and how long from its return until the grantee first reads
/// the document.
async fn grant_latencies(
    provider: DocumentAcp,
) -> anyhow::Result<Vec<(Duration, Option<Duration>)>> {
    let household = Household::open(provider).await?;
    let node = household.as_node();
    let task = super::create_task(&node, "latency").await?;
    household
        .settled(&household.node_did, "the Task's registration")
        .await?;
    let mut latencies = Vec::with_capacity(GRANTS);
    for index in 0..GRANTS {
        let grantee = household.held_key(&format!("grantee-{index}.key")).await?;
        let grantee =
            identity::Did::new(gents_store::identity::NodeIdentity::did(grantee.as_ref()))?;
        let begun = Instant::now();
        node.transact_write("test.latency.grant", |txn| {
            let (task, grantee) = (&task, &grantee);
            Box::pin(async move {
                txn.add_actor_relationship("Task", task, "reader", grantee)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
        let committed = Instant::now();
        let commit = committed - begun;
        let effective = household
            .holds_within(committed, &grantee, "read", "configuration", &task)
            .await?;
        tracing::info!(%provider, index, ?commit, ?effective, "staged grant effective");
        latencies.push((commit, effective));
    }
    household.shutdown().await?;
    Ok(latencies)
}

fn summary(latencies: &[Duration]) -> String {
    let mut sorted = latencies.to_vec();
    sorted.sort();
    let at = |quantile: f64| sorted[((sorted.len() - 1) as f64 * quantile).round() as usize];
    format!(
        "min {:?} median {:?} p90 {:?} max {:?}",
        sorted[0],
        at(0.5),
        at(0.9),
        sorted[sorted.len() - 1]
    )
}

/// Every staged grant becomes effective; how long it takes is recorded, not
/// asserted.
#[tokio::test]
async fn a_staged_grant_becomes_effective_for_its_grantee() -> anyhow::Result<()> {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    for provider in PROVIDERS {
        let latencies = grant_latencies(provider).await?;
        let commits: Vec<Duration> = latencies.iter().map(|(commit, _)| *commit).collect();
        let effective: Vec<Duration> = latencies
            .iter()
            .filter_map(|(_, effective)| *effective)
            .collect();
        assert_eq!(effective.len(), GRANTS, "{provider}: {latencies:?}");
        tracing::info!(
            %provider,
            commit = summary(&commits),
            after_commit = summary(&effective),
            "staged grant latency"
        );
    }
    Ok(())
}
