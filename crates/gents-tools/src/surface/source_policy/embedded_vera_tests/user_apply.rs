use gents_documents::test_support::desired_state_fixtures::{backend, config, document};
use gents_documents::{apply_desired_state_plan, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde_json::json;

use crate::test_support::vera_test_home::{seen, Household, PROVIDERS};

/// What an apply that configures an Agent leaves: its outcome, the rows
/// stored, whether the Agent's row exists and whether it executes as itself.
#[derive(Debug, PartialEq, Eq)]
struct Applied {
    outcome: Result<(), String>,
    tools: Vec<String>,
    contexts: Vec<String>,
    agent_row: bool,
    executor: bool,
}

async fn apply(household: &Household, access: &ConfigAccess) -> anyhow::Result<Applied> {
    let node = household.node_did.as_str();
    let agent = household.derive_agent()?;
    let plan = DesiredStateApplyPlan::new(vec![
        document(backend(node, "backend")),
        config(
            Collection::InferenceProfile,
            json!({"node_did": node, "profile_id": "fixture", "backend_id": "backend", "model_name": "model"}),
        ),
        config(
            Collection::Tools,
            json!({"node_did": node, "tools_id": "user-apply-tools"}),
        ),
        config(
            Collection::AgentContext,
            json!({"node_did": node, "context_id": "user-apply-context", "tools_id": "user-apply-tools"}),
        ),
        config(
            Collection::Agent,
            json!({"node_did": node, "agent_did": agent.agent_did,
                "creation_nonce": agent.creation_nonce, "context_id": "user-apply-context",
                "inference_profile_id": "fixture", "enabled": true}),
        ),
    ])?;
    let outcome = access
        .transact_write("test.user_apply", |txn| {
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
        .map_err(|error| seen(error.as_ref()));
    household
        .owed_until(&household.user_did, |owed| {
            owed.iter()
                .all(|entry| entry.state != acp::OwedState::Staged)
        })
        .await?;
    let stored = |collection: &'static str, field: &'static str| {
        super::stored(&household.node, &household.user_did, collection, field)
    };
    let agent_docs = stored("Agent", "_docID").await?;
    let executor = match agent_docs.first() {
        Some(doc) => {
            household
                .holds(
                    &identity::Did::new(&agent.agent_did)?,
                    "executor",
                    "agents",
                    doc,
                )
                .await?
        }
        None => false,
    };
    Ok(Applied {
        outcome,
        tools: stored("Tools", "tools_id").await?,
        contexts: stored("AgentContext", "context_id").await?,
        agent_row: stored("Agent", "agent_did")
            .await?
            .contains(&agent.agent_did),
        executor,
    })
}

/// Lean `Identity.UserDelegation.a_user_write_on_vera_is_forwarded_or_refused`:
/// the User's client presents a delegation with every write, so its apply is
/// forwarded, and the new Agent's `parent` and `executor` are the User's own
/// grants on its own document. Over HTTP the apply commits whole on either
/// kind of home; in the runtime's process only the Node and managed Agents
/// mutate, so the same apply as the User is refused and stores nothing.
#[tokio::test]
async fn a_users_apply_of_an_agents_configuration_commits_whole_over_http() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let household = Household::open(provider).await?;
        let in_process = apply(
            &household,
            &ConfigAccess::local_as(household.node.clone(), household.user_did.clone()),
        )
        .await?;
        let client = apply(&household, &household.as_user()?).await?;
        household.shutdown().await?;
        assert_eq!(
            in_process,
            Applied {
                outcome: Err("validated managed Agent signing capability is required".to_owned()),
                tools: Vec::new(),
                contexts: Vec::new(),
                agent_row: false,
                executor: false,
            },
            "{provider}"
        );
        assert_eq!(
            client,
            Applied {
                outcome: Ok(()),
                tools: vec!["user-apply-tools".to_owned()],
                contexts: vec!["user-apply-context".to_owned()],
                agent_row: true,
                executor: true,
            },
            "{provider}"
        );
    }
    Ok(())
}
