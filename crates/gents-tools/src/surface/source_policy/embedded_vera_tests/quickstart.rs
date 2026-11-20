use gents_store::config_client::TransactionFailure as _;
use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::{seen, Household, PROVIDERS};

/// What quickstart does when the User selects a runtime-created Agent as a
/// workspace's writer: whether the store denied it, the
/// placements stored, and whether the selection took.
#[derive(Debug, PartialEq, Eq)]
struct Selected {
    refused_as_denied: bool,
    placements: usize,
    selected: bool,
}

async fn quickstart(provider: DocumentAcp) -> anyhow::Result<(Selected, Result<(), String>)> {
    let household = Household::open(provider).await?;
    let (engineer, _) = household.engineer().await?;
    let worker = household.derive_agent()?;
    let worker_doc = household.agent_creates_agent(&engineer, &worker).await?;
    let repository = household.directory.path().join("repository");
    tokio::fs::create_dir(&repository).await?;
    let selection = gents_session::workspace::WorkspaceProvisioningSelection {
        workspace_id: uuid::Uuid::new_v4().to_string(),
        node_did: household.node_did.to_string(),
        creator_agent_did: engineer.agent_did.clone(),
        writer_agent_did: worker.agent_did.clone(),
        integrator_agent_did: worker.agent_did.clone(),
        reader_agent_dids: Vec::new(),
    };
    let outcome = gents_session::workspace::provision_read_only_workspace(
        &household.as_user()?,
        &household.agent_access(&engineer)?,
        &repository,
        "0000000000000000000000000000000000000000",
        &selection,
    )
    .await
    .map(|_| ());
    let denied = outcome
        .as_ref()
        .is_err_and(|error| error.permission_denied());
    let outcome = outcome.map_err(|error| seen(error.as_ref()));
    household
        .owed_until(&household.user_did, |owed| {
            owed.iter()
                .all(|entry| entry.state != acp::OwedState::Staged)
        })
        .await?;
    let selected = Selected {
        refused_as_denied: denied,
        placements: super::stored(
            &household.node,
            &household.user_did,
            "RepositoryPlacement",
            "repository_id",
        )
        .await?
        .len(),
        selected: household
            .holds(
                &identity::Did::new(&engineer.agent_did)?,
                "workspace_selected_by",
                "agents",
                &worker_doc,
            )
            .await?,
    };
    household.shutdown().await?;
    Ok((selected, outcome))
}

/// Lean `Identity.Household.mayManage`: nothing manages
/// `agents#workspace_selected_by`, so only the selected Agent document's
/// owner may write it, and a runtime-created Agent's owner is the Agent that
/// created it, not the User. Quickstart is refused whole on either kind of
/// home.
#[tokio::test]
async fn quickstart_selection_of_a_runtime_owned_agent_is_refused_whole() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let (selected, outcome) = quickstart(provider).await?;
        assert_eq!(
            selected,
            Selected {
                refused_as_denied: true,
                placements: 0,
                selected: false,
            },
            "{provider}: {outcome:?}"
        );
    }
    Ok(())
}
