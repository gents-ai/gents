use gents_protocol::owner_staged::OwnerStagedChange;
use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::{describe, seen, Household, PROVIDERS};

/// What the User's owner-staged grant leaves on configuration a
/// runtime-created Agent owns: the grant's outcome, whether the User's next
/// command nonce is fenced, whether using that nonce again is refused, whether
/// the grantee then writes the cursor, and what the owner still owes the
/// provider.
#[derive(Debug, PartialEq, Eq)]
struct Granted {
    grant: Result<(), String>,
    nonce: Result<(), String>,
    replay_refused: bool,
    worker_writes_cursor: bool,
    owed: Vec<String>,
}

async fn grant(provider: DocumentAcp) -> anyhow::Result<Granted> {
    let household = Household::open(provider).await?;
    let (engineer, _) = household.engineer().await?;
    let worker = household.derive_agent()?;
    household.agent_creates_agent(&engineer, &worker).await?;
    let engineer_did = identity::Did::new(&engineer.agent_did)?;
    let cursor = household
        .agent_access(&engineer)?
        .transact_write("test.owner_staged.cursor", |txn| {
            let owner = engineer_did.clone();
            Box::pin(async move {
                let created = txn
                    .execute_with_variables(
                        "mutation($input: EventSourceCursorMutationInputArg!) { create_EventSourceCursor(input: $input) { _docID } }",
                        &serde_json::json!({"input": {
                            "cursor_key": "owner-staged",
                            "owner_did": owner.as_str(),
                            "source_collection": "Task",
                        }}),
                    )
                    .await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                    &created,
                    "EventSourceCursor",
                )?)
            })
        })
        .await?;
    household
        .settled(&engineer_did, "the Engineer's cursor")
        .await?;

    let change = OwnerStagedChange::EventCursorWriter {
        cursor_doc_id: cursor.clone(),
        agent_did: worker.agent_did.clone(),
    };
    let grant = household
        .as_user()?
        .request_owner_staged(&change, true)
        .await
        .map_err(|error| seen(&error));
    let nonce = gents_store::config_client::consume_signed_command_nonce(
        &household.node,
        household.user_did.as_str(),
        "owner-staged-nonce",
        chrono::Utc::now(),
    )
    .await
    .map_err(|error| seen(&error));
    let replay_refused = matches!(
        gents_store::config_client::consume_signed_command_nonce(
            &household.node,
            household.user_did.as_str(),
            "owner-staged-nonce",
            chrono::Utc::now(),
        )
        .await,
        Err(gents_store::config_client::SignedCommandNonceError::Consumed)
    );
    let owed = household
        .owed_until(&engineer_did, |owed| {
            owed.iter()
                .all(|entry| entry.state != acp::OwedState::Staged)
        })
        .await?
        .iter()
        .map(describe)
        .collect();
    let worker_writes_cursor = household
        .holds(
            &identity::Did::new(&worker.agent_did)?,
            "writer",
            "configuration",
            &cursor,
        )
        .await?;
    household.shutdown().await?;
    Ok(Granted {
        grant,
        nonce,
        replay_refused,
        worker_writes_cursor,
        owed,
    })
}

/// Lean `Identity.OwnerStaged.ownerStaged` stages an owner-managed change as
/// the document's owner when that owner is the runtime or one of its Agents
/// (`staged_owner_is_runtime_or_its_agent`): the User's command is decided,
/// its replay fence persists, and the grant takes effect on either kind of
/// home.
#[tokio::test]
async fn a_users_owner_staged_grant_takes_effect() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        assert_eq!(
            grant(provider).await?,
            Granted {
                grant: Ok(()),
                nonce: Ok(()),
                replay_refused: true,
                worker_writes_cursor: true,
                owed: Vec::new(),
            },
            "{provider}"
        );
    }
    Ok(())
}
