use gents_store::config_client::ConfigAccess;
use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::{seen, Household, PROVIDERS};

/// A Task the Node creates, as it creates operational configuration, whose
/// `selection_manager` it publishes to the Node's owner and administrators
/// and, when given, to the config writers of one Agent.
async fn selection(
    household: &Household,
    name: &str,
    config_writers_of: Option<&str>,
) -> anyhow::Result<String> {
    let node_doc = household.node_doc.clone();
    let task = household
        .as_node()
        .transact_write("test.managers.selection", |txn| {
            let node_doc = node_doc.clone();
            Box::pin(async move {
                let created = txn
                    .execute_with_variables(
                        "mutation($task: String) { create_Task(input: {task_id: $task}) { _docID } }",
                        &serde_json::json!({ "task": name }),
                    )
                    .await?;
                let task = gents_store::graphql::created_doc_id(&created, "Task")?;
                gents_store::config_client::publish_node_managers_in_txn(
                    txn,
                    "Task",
                    &task,
                    "selection_manager",
                    &node_doc,
                )
                .await?;
                if let Some(agent_doc) = config_writers_of {
                    txn.add_entity_set_relationship(
                        "Task",
                        &task,
                        "selection_manager",
                        "agents",
                        agent_doc,
                        "config_writer",
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(task)
            })
        })
        .await?;
    household
        .settled(&household.node_did, "the selection's managers")
        .await?;
    Ok(task)
}

/// Wait until `actor` owes nothing still to be sent.
async fn sent(household: &Household, actor: &ConfigAccess) -> anyhow::Result<()> {
    if let Some(did) = actor.actor_did() {
        let owed = household
            .owed_until(&identity::Did::new(&did)?, |owed| {
                owed.iter()
                    .all(|entry| matches!(entry.state, acp::OwedState::Refused { .. }))
            })
            .await?;
        anyhow::ensure!(
            owed.iter()
                .all(|entry| matches!(entry.state, acp::OwedState::Refused { .. })),
            "still owed: {owed:?}"
        );
    }
    Ok(())
}

/// Select `agent_doc`'s executors on `task` as an in-process `actor`, in a
/// transaction: the outcome its caller sees, and whether the selected Agent
/// holds `selected_by` once settled.
async fn select(
    household: &Household,
    actor: &ConfigAccess,
    task: &str,
    agent_doc: &str,
    selected: &identity::Did,
) -> anyhow::Result<(Result<(), String>, bool)> {
    let outcome = actor
        .transact_write("test.managers.select", |txn| {
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "Task",
                    task,
                    "selected_by",
                    "agents",
                    agent_doc,
                    "executor",
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .map_err(|error| seen(error.as_ref()));
    sent(household, actor).await?;
    Ok((
        outcome,
        household
            .holds(selected, "selected_by", "configuration", task)
            .await?,
    ))
}

/// Grant `reader` on an Agent document as a client `actor`, outside any
/// transaction: the outcome, and whether the reader holds it once settled.
async fn administer(
    household: &Household,
    actor: &ConfigAccess,
    agent_doc: &str,
    reader: &identity::Did,
) -> anyhow::Result<(Result<(), String>, bool)> {
    let outcome = actor
        .add_actor_relationship("Agent", agent_doc, "reader", reader)
        .await
        .map_err(|error| seen(&error));
    sent(household, actor).await?;
    Ok((
        outcome,
        household
            .holds(reader, "reader", "agents", agent_doc)
            .await?,
    ))
}

async fn managers(
    provider: DocumentAcp,
) -> anyhow::Result<Vec<(&'static str, Result<(), String>, bool)>> {
    let household = Household::open(provider).await?;
    let (engineer, engineer_doc) = household.engineer().await?;
    let selected = household.derive_agent()?;
    let selected_doc = household.create_agent(&selected).await?;
    let selected_did = identity::Did::new(&selected.agent_did)?;
    let writer = household.derive_agent()?;
    household.create_agent(&writer).await?;
    let stranger_agent = household.derive_agent()?;
    household.create_agent(&stranger_agent).await?;
    let created = household.derive_agent()?;
    let created_doc = household.agent_creates_agent(&engineer, &created).await?;

    let member = household.client("member.key").await?;
    let stranger = household.client("stranger.key").await?;
    let user = household.as_user()?;
    user.add_actor_relationship(
        "Node",
        &household.node_doc,
        "administrator",
        &identity::Did::new(member.did())?,
    )
    .await?;
    user.add_actor_relationship(
        "Agent",
        &engineer_doc,
        "config_writer",
        &identity::Did::new(&writer.agent_did)?,
    )
    .await?;
    household
        .settled(&household.user_did, "the managers' grants")
        .await?;

    let mut seen_by = Vec::new();
    let selections: [(&'static str, ConfigAccess, Option<&str>); 4] = [
        ("owner selects", household.as_node(), None),
        (
            "nodes#administrator selects",
            household.agent_access(&engineer)?,
            None,
        ),
        (
            "agents#config_writer selects",
            household.agent_access(&writer)?,
            Some(engineer_doc.as_str()),
        ),
        (
            "stranger Agent selects",
            household.agent_access(&stranger_agent)?,
            None,
        ),
    ];
    for (name, actor, config_writers_of) in selections {
        let task = selection(&household, name, config_writers_of).await?;
        let (outcome, holds) =
            select(&household, &actor, &task, &selected_doc, &selected_did).await?;
        seen_by.push((name, outcome, holds));
    }
    let administrators: [(&'static str, ConfigAccess); 4] = [
        ("owner administers", household.agent_access(&engineer)?),
        ("nodes#owner administers", household.as_user()?),
        (
            "nodes#administrator administers",
            household.as_client(&member)?,
        ),
        ("stranger User administers", household.as_client(&stranger)?),
    ];
    for (index, (name, actor)) in administrators.into_iter().enumerate() {
        let reader = household.held_key(&format!("reader-{index}.key")).await?;
        let reader = identity::Did::new(gents_store::identity::NodeIdentity::did(reader.as_ref()))?;
        let (outcome, holds) = administer(&household, &actor, &created_doc, &reader).await?;
        seen_by.push((name, outcome, holds));
    }
    household.shutdown().await?;
    Ok(seen_by)
}

/// Lean `Identity.Household.mayManage` (`node_owner_and_admin_manage_selection`):
/// the owner, and every holder of a managing relation reached through the
/// usersets `nodes#owner`, `nodes#administrator` and `agents#config_writer`,
/// may change the managed relation; nobody else may.
#[tokio::test]
async fn vera_node_owner_userset_manages_selection() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let seen_by = managers(provider).await?;
        let decided: Vec<_> = seen_by
            .iter()
            .map(|(name, outcome, holds)| (*name, outcome.is_ok(), *holds))
            .collect();
        assert_eq!(
            decided,
            [
                ("owner selects", true, true),
                ("nodes#administrator selects", true, true),
                ("agents#config_writer selects", true, true),
                ("stranger Agent selects", false, false),
                ("owner administers", true, true),
                ("nodes#owner administers", true, true),
                ("nodes#administrator administers", true, true),
                ("stranger User administers", false, false),
            ],
            "{provider}: {seen_by:?}"
        );
    }
    Ok(())
}
