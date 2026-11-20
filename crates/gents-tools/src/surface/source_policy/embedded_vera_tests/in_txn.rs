use gents_store::config_client::{ConfigAccess, TransactionFailure as _};

use crate::test_support::vera_test_home::{describe, seen, Household, PROVIDERS};

/// What is left after one actor, in one transaction, creates a Task of its
/// own and grants an Agent `reader` on a Task the Engineer owns: whether the
/// caller saw the store deny it, the Tasks stored, what
/// the actor still owes the provider, and whether the grantee reads the
/// Engineer's Task.
#[derive(Debug, PartialEq, Eq)]
struct Left {
    refused_as_denied: bool,
    tasks: Vec<String>,
    owed: Vec<String>,
    grantee_reads: bool,
}

async fn cross_owner_grant(
    household: &Household,
    actor: &ConfigAccess,
    row: &'static str,
    engineers: &str,
    grantee: &identity::Did,
) -> anyhow::Result<(Left, Option<String>)> {
    let outcome = actor
        .transact_write("test.in_txn.cross_owner_reader", |txn| {
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($task: String) { create_Task(input: {task_id: $task}) { _docID } }",
                    &serde_json::json!({ "task": row }),
                )
                .await?;
                txn.add_actor_relationship("Task", engineers, "reader", grantee)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await;
    let denied = outcome
        .as_ref()
        .is_err_and(|error| error.permission_denied());
    let error = outcome.as_ref().err().map(|error| seen(error.as_ref()));
    let actor_did = identity::Did::new(
        actor
            .actor_did()
            .ok_or_else(|| anyhow::anyhow!("the actor has a DID"))?,
    )?;
    let owed = household
        .owed_until(&actor_did, |owed| {
            owed.iter()
                .all(|entry| entry.state != acp::OwedState::Staged)
        })
        .await?;
    let mut tasks = super::tasks(&household.node, &household.user_did).await?;
    tasks.sort();
    Ok((
        Left {
            refused_as_denied: denied,
            tasks,
            owed: owed.iter().map(describe).collect(),
            grantee_reads: household
                .holds(grantee, "read", "configuration", engineers)
                .await?,
        },
        error,
    ))
}

/// Lean `Identity.Household.mayManage`: nothing manages `reader` of
/// configuration, so only its owner changes it; Lean
/// `Identity.AgentTargetAccess.reconcile_refuses_without_authority` refuses
/// the whole change, leaving nothing of it. Neither the User's client, in
/// its HTTP transaction, nor another Agent owns the Engineer's Task.
#[tokio::test]
async fn a_cross_owner_reader_grant_in_a_transaction_is_refused_whole() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let household = Household::open(provider).await?;
        let (engineer, _) = household.engineer().await?;
        let other = household.derive_agent()?;
        household.create_agent(&other).await?;
        let grantee = identity::Did::new(&household.derive_agent()?.agent_did)?;
        let engineers =
            super::create_task(&household.agent_access(&engineer)?, "engineer-row").await?;
        household
            .settled(
                &identity::Did::new(&engineer.agent_did)?,
                "the Engineer's Task",
            )
            .await?;
        let actors = [
            ("user-row", household.as_user()?),
            ("agent-row", household.agent_access(&other)?),
        ];
        let mut seen_by = Vec::new();
        for (row, actor) in actors {
            seen_by.push((
                row,
                cross_owner_grant(&household, &actor, row, &engineers, &grantee).await?,
            ));
        }
        household.shutdown().await?;
        for (row, (left, error)) in seen_by {
            assert_eq!(
                left,
                Left {
                    refused_as_denied: true,
                    tasks: vec!["engineer-row".to_owned()],
                    owed: Vec::new(),
                    grantee_reads: false,
                },
                "{provider} {row}: {error:?}"
            );
        }
    }
    Ok(())
}
