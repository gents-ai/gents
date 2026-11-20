use gents_documents::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::local::document_acp::DocumentAcp;
use serde_json::{json, Value};

use crate::test_support::vera_test_home::{seen, Household, PROVIDERS};

fn document(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

async fn apply(
    access: &ConfigAccess,
    documents: Vec<DesiredStateApplyDocument>,
) -> Result<(), String> {
    let plan = DesiredStateApplyPlan::new(documents).map_err(|error| seen(error.as_ref()))?;
    access
        .transact_write("test.rebinding.apply", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .map_err(|error| seen(error.as_ref()))
}

/// Event automation bound to `agent`.
fn automation(node_did: &str, agent: &str) -> Vec<DesiredStateApplyDocument> {
    vec![
        document(
            Collection::Task,
            json!({"node_did": node_did, "task_id": "task", "agent_did": agent, "prompt_template": "hello"}),
        ),
        document(
            Collection::EventSource,
            json!({"node_did": node_did, "event_source_id": "source", "source_collection": "Task", "event_kind": "created"}),
        ),
        document(
            Collection::Trigger,
            json!({"node_did": node_did, "trigger_id": "trigger", "task_id": "task", "source": {"kind": "event", "event_source_id": "source"}}),
        ),
    ]
}

/// What the User's rebinding of automation from one Agent to another leaves:
/// each apply's outcome, and which Agent then reads the automation's Task.
#[derive(Debug, PartialEq, Eq)]
struct Rebound {
    published: Result<(), String>,
    unchanged: Result<(), String>,
    rebound: Result<(), String>,
    first_reads: bool,
    second_reads: bool,
}

async fn rebind(provider: DocumentAcp, engineer_publishes: bool) -> anyhow::Result<Rebound> {
    let household = Household::open(provider).await?;
    let (engineer, _) = household.engineer().await?;
    let (first, second) = (household.derive_agent()?, household.derive_agent()?);
    for agent in [&first, &second] {
        household.create_agent(agent).await?;
    }
    let node_did = household.node_did.to_string();
    let user = household.as_user()?;
    let (publisher, publisher_did) = if engineer_publishes {
        (
            household.agent_access(&engineer)?.with_runtime_agent_proof(
                gents_store::identity::RuntimeAgentProof::new(household.home.node.clone()),
            ),
            identity::Did::new(&engineer.agent_did)?,
        )
    } else {
        (user.clone(), household.user_did.clone())
    };
    let published = apply(&publisher, automation(&node_did, &first.agent_did)).await;
    household
        .settled(&publisher_did, "the published automation")
        .await?;
    let unchanged = apply(&user, automation(&node_did, &first.agent_did)).await;
    let rebound = apply(&user, automation(&node_did, &second.agent_did)).await;
    household
        .owed_until(&household.user_did, |owed| {
            owed.iter()
                .all(|entry| entry.state != acp::OwedState::Staged)
        })
        .await?;
    let task = super::stored(&household.node, &household.user_did, "Task", "_docID")
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("the automation's Task is stored"))?;
    let reads = |agent: &gents_protocol::agent_identity::AgentIdentityBinding| {
        let (household, task) = (&household, &task);
        let agent = agent.agent_did.clone();
        async move {
            household
                .holds(&identity::Did::new(&agent)?, "read", "configuration", task)
                .await
        }
    };
    let (first_reads, second_reads) = (reads(&first).await?, reads(&second).await?);
    household.shutdown().await?;
    Ok(Rebound {
        published,
        unchanged,
        rebound,
        first_reads,
        second_reads,
    })
}

/// Lean `Identity.AgentTargetAccess`: an unchanged configuration is inert
/// (`unchanged_configuration_is_inert`), a configurer rebinds what it owns,
/// and a rebinding whose relations the configurer does not manage is refused
/// whole (`reconcile_refuses_without_authority`): nothing manages `reader` of
/// configuration the Engineer owns. Each holds on either kind of home.
#[tokio::test]
async fn the_users_rebinding_of_automation_takes_only_on_what_it_owns() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        assert_eq!(
            rebind(provider, false).await?,
            Rebound {
                published: Ok(()),
                unchanged: Ok(()),
                rebound: Ok(()),
                first_reads: false,
                second_reads: true,
            },
            "{provider}: the User's own automation"
        );
        let engineers = rebind(provider, true).await?;
        assert_eq!(
            (
                &engineers.published,
                &engineers.unchanged,
                engineers.rebound.is_err(),
                engineers.first_reads,
                engineers.second_reads,
            ),
            (&Ok(()), &Ok(()), true, true, false),
            "{provider}: the Engineer's automation: {engineers:?}"
        );
    }
    Ok(())
}
