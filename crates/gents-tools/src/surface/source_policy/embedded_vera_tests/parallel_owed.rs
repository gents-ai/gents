use std::time::{Duration, Instant};

use gents_store::config_client::ConfigAccess;
use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::{describe, Household};

const GRANTS_PER_ACTOR: usize = 4;
const AGENTS: usize = 3;
const ROUNDS: usize = 3;

/// What one round leaves: how long the parallel commits took, how long from
/// the last commit until no actor owes anything (none if something is still
/// owed at the ceiling), what is still owed, and how many grantees read.
#[derive(Debug)]
struct Round {
    committed: Duration,
    settled: Option<Duration>,
    owed: Vec<String>,
    reading: usize,
}

/// Every actor grants `reader` on its own Task to `GRANTS_PER_ACTOR`
/// grantees, all commits at once.
async fn parallel_round(
    household: &Household,
    actors: &[(identity::Did, ConfigAccess)],
    round: usize,
) -> anyhow::Result<Round> {
    let mut grants = Vec::new();
    for (index, (did, access)) in actors.iter().enumerate() {
        let task = super::create_task(access, &format!("parallel-{round}-{index}")).await?;
        household.settled(did, "the Task's registration").await?;
        for grant in 0..GRANTS_PER_ACTOR {
            let key = household
                .held_key(&format!("parallel-{round}-{index}-{grant}.key"))
                .await?;
            let grantee =
                identity::Did::new(gents_store::identity::NodeIdentity::did(key.as_ref()))?;
            grants.push((access.clone(), task.clone(), grantee));
        }
    }
    let begun = Instant::now();
    let commits = grants.iter().map(|(access, task, grantee)| async move {
        access
            .transact_write("test.parallel_owed.grant", |txn| {
                Box::pin(async move {
                    txn.add_actor_relationship("Task", task, "reader", grantee)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
    });
    for outcome in futures::future::join_all(commits).await {
        outcome?;
    }
    let committed = begun.elapsed();
    let settle_started = Instant::now();
    let mut owed = Vec::new();
    for (did, _) in actors {
        owed.extend(
            household
                .owed_until(did, Vec::is_empty)
                .await?
                .iter()
                .map(describe),
        );
    }
    let settled = owed.is_empty().then(|| settle_started.elapsed());
    let mut reading = 0;
    for (_, task, grantee) in &grants {
        if household
            .holds(grantee, "read", "configuration", task)
            .await?
        {
            reading += 1;
        }
    }
    Ok(Round {
        committed,
        settled,
        owed,
        reading,
    })
}

/// Relationship changes the Node and several managed Agents commit in
/// parallel on a Vera home are all settled and all take effect; how long it
/// takes is recorded, not asserted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_relationship_changes_on_a_vera_home_all_settle() -> anyhow::Result<()> {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    let household = Household::open(DocumentAcp::Vera).await?;
    let mut actors = vec![(household.node_did.clone(), household.as_node())];
    for _ in 0..AGENTS {
        let agent = household.derive_agent()?;
        household.create_agent(&agent).await?;
        actors.push((
            identity::Did::new(&agent.agent_did)?,
            household.agent_access(&agent)?,
        ));
    }
    let grants = actors.len() * GRANTS_PER_ACTOR;
    let mut rounds = Vec::new();
    for round in 0..ROUNDS {
        let seen = parallel_round(&household, &actors, round).await?;
        tracing::info!(
            round,
            grants,
            committed = ?seen.committed,
            settled = ?seen.settled,
            ?seen,
            "parallel relationship changes"
        );
        rounds.push(seen);
    }
    household.shutdown().await?;
    for seen in &rounds {
        assert!(
            seen.settled.is_some() && seen.owed.is_empty() && seen.reading == grants,
            "{rounds:?}"
        );
    }
    Ok(())
}

/// Relationship changes the User's client sends in parallel over HTTP, each
/// outside any transaction, on Agent documents the User created: all are
/// settled and all take effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_client_relationship_changes_on_a_vera_home_all_settle() -> anyhow::Result<()> {
    let _logs =
        tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
    let household = Household::open(DocumentAcp::Vera).await?;
    let mut agents = Vec::new();
    for _ in 0..AGENTS {
        let agent = household.derive_agent()?;
        agents.push(household.create_agent(&agent).await?);
    }
    let user = household.as_user()?;
    let mut rounds = Vec::new();
    for round in 0..ROUNDS {
        let mut grants = Vec::new();
        for (index, agent_doc) in agents.iter().enumerate() {
            for grant in 0..GRANTS_PER_ACTOR {
                let key = household
                    .held_key(&format!("client-{round}-{index}-{grant}.key"))
                    .await?;
                let grantee =
                    identity::Did::new(gents_store::identity::NodeIdentity::did(key.as_ref()))?;
                grants.push((agent_doc.clone(), grantee));
            }
        }
        let begun = Instant::now();
        let sent = grants.iter().map(|(agent_doc, grantee)| {
            let user = &user;
            async move {
                user.add_actor_relationship("Agent", agent_doc, "reader", grantee)
                    .await
            }
        });
        let failed: Vec<String> = futures::future::join_all(sent)
            .await
            .into_iter()
            .filter_map(|outcome| {
                outcome
                    .err()
                    .map(|error| crate::test_support::vera_test_home::seen(&error))
            })
            .collect();
        let committed = begun.elapsed();
        let settle_started = Instant::now();
        let owed: Vec<String> = household
            .owed_until(&household.user_did, Vec::is_empty)
            .await?
            .iter()
            .map(describe)
            .collect();
        let settled = owed.is_empty().then(|| settle_started.elapsed());
        let mut reading = 0;
        for (agent_doc, grantee) in &grants {
            if household
                .holds(grantee, "reader", "agents", agent_doc)
                .await?
            {
                reading += 1;
            }
        }
        tracing::info!(
            round,
            grants = grants.len(),
            ?committed,
            ?settled,
            ?owed,
            ?failed,
            reading,
            "parallel client relationship changes"
        );
        rounds.push((failed, settled.is_some(), owed, reading == grants.len()));
    }
    household.shutdown().await?;
    for (failed, settled, owed, all_read) in &rounds {
        assert!(
            failed.is_empty() && *settled && owed.is_empty() && *all_read,
            "{rounds:?}"
        );
    }
    Ok(())
}
