use crate::test_support::vera_home::{describe, seen, Household, PROVIDERS};

/// What is left after another Agent, in one transaction, creates a Task of
/// its own and grants a third Agent `reader` on a Task the Engineer owns:
/// whether the caller saw the store refuse it as not the owner, the Tasks
/// stored, what the Agent still owes the provider, and whether the grantee
/// reads the Engineer's Task.
#[derive(Debug, PartialEq, Eq)]
struct Left {
    refused_as_not_the_owner: bool,
    tasks: Vec<String>,
    owed: Vec<String>,
    grantee_reads: bool,
}

/// Lean `Identity.Household.mayManage`: nothing manages `reader` of
/// configuration, so only its owner changes it; Lean
/// `Identity.AgentTargetAccess.reconcile_refuses_without_authority` refuses
/// the whole change, leaving nothing of it. Another Agent does not own the
/// Engineer's Task.
#[tokio::test]
async fn an_agents_cross_owner_reader_grant_in_a_transaction_is_refused_whole() -> anyhow::Result<()>
{
    for provider in PROVIDERS {
        let household = Household::open(provider).await?;
        let (engineer, _) = household.engineer().await?;
        let other = household.derive_agent()?;
        household.create_agent(&other).await?;
        let other_did = identity::Did::new(&other.agent_did)?;
        let grantee = identity::Did::new(&household.derive_agent()?.agent_did)?;
        let engineers =
            super::create_task(&household.agent_access(&engineer)?, "engineer-row").await?;
        household
            .settled(
                &identity::Did::new(&engineer.agent_did)?,
                "the Engineer's Task",
            )
            .await?;
        let outcome = household
            .agent_access(&other)?
            .transact_write("test.in_txn.cross_owner_reader", |txn| {
                let (engineers, grantee) = (&engineers, &grantee);
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($task: String) { create_Task(input: {task_id: $task}) { _docID } }",
                        &serde_json::json!({ "task": "agent-row" }),
                    )
                    .await?;
                    txn.add_actor_relationship("Task", engineers, "reader", grantee)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        let error = outcome.as_ref().err().map(|error| seen(error.as_ref()));
        let owed = household
            .owed_until(&other_did, |owed| {
                owed.iter()
                    .all(|entry| entry.state != acp::OwedState::Staged)
            })
            .await?;
        let mut tasks = super::tasks(&household.node, &household.user_did).await?;
        tasks.sort();
        let left = Left {
            refused_as_not_the_owner: error
                .as_deref()
                .is_some_and(|error| error.contains("not document owner")),
            tasks,
            owed: owed.iter().map(describe).collect(),
            grantee_reads: household
                .holds(&grantee, "read", "configuration", &engineers)
                .await?,
        };
        household.shutdown().await?;
        assert_eq!(
            left,
            Left {
                refused_as_not_the_owner: true,
                tasks: vec!["engineer-row".to_owned()],
                owed: Vec::new(),
                grantee_reads: false,
            },
            "{provider}: {error:?}"
        );
    }
    Ok(())
}
