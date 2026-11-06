use gents_protocol::goal_control::{SessionGoalControl, SessionGoalControlReceipt};

use super::*;
use crate::store::config_client::ConfigAccess;
use crate::store::identity::NodeIdentity;

mod provenance;
#[cfg(test)]
mod tests;
pub use provenance::{
    load_applied_opening_goal_id, load_applied_opening_goal_ids,
    load_applied_opening_goal_provenance, OpeningGoalProvenance,
};

/// Publishes one author-signed intent. The selected agent's receipt confirms
/// the transaction; observing its Goal is a separate read.
pub async fn submit_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    command: SessionGoalControl,
) -> Result<SessionGoalControlReceipt> {
    let command = prepare_session_goal_control(access, signer, command).await?;
    submit_prepared_session_goal_control(access, signer, &command).await
}

/// Preparation has no database effect. A caller retaining this command must
/// commit it to its existing encrypted journal before publication is allowed.
pub async fn prepare_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    mut command: SessionGoalControl,
) -> Result<SessionGoalControl> {
    anyhow::ensure!(
        command.requester_did == signer.did()
            && access.actor_did().as_deref() == Some(signer.did()),
        "goal control signer differs from its author"
    );
    command.native_evidence = if let Some(request) = command.initial_request.as_deref() {
        anyhow::ensure!(
            matches!(
                &command.operation,
                gents_protocol::goal_control::GoalOperation::Create { .. }
            ) && request.admission.signer_did == command.requester_did
                && request.session_doc_id == command.session_doc_id
                && request.agent_did == command.agent_did
                && request.agent_doc_id.as_deref() == Some(command.agent_doc_id.as_str()),
            "goal request differs from its signed command scope"
        );
        let input = crate::store::config_client::client_request_input(request)?;
        let request = crate::store::config_client::prepare_request_evidence(signer, input).await?;
        let replacement_delete = match command.expected.as_ref() {
            Some(revision) => Some(
                crate::store::config_client::prepare_goal_delete_evidence(
                    access,
                    signer,
                    &revision.document,
                )
                .await?,
            ),
            None => None,
        };
        Some(gents_protocol::goal_control::GoalNativeEvidence {
            request,
            replacement_delete,
        })
    } else {
        None
    };
    command.signature = signer.sign(&command.signing_payload()?).await?;
    command.validate_shape()?;
    Ok(command)
}

async fn validate_prepared(signer: &dyn NodeIdentity, command: &SessionGoalControl) -> Result<()> {
    command.validate_shape()?;
    anyhow::ensure!(
        command.requester_did == signer.did()
            && signer
                .verify(
                    &command.requester_did,
                    &command.signing_payload()?,
                    &command.signature,
                )
                .await?,
        "retained goal command does not match its authenticated author"
    );
    Ok(())
}

async fn retained_control(
    access: &ConfigAccess,
    command: &SessionGoalControl,
) -> Result<Option<serde_json::Value>> {
    let response = access.execute(&format!(
        r#"{{ AgentSessionGoalControl(filter: {{ command_id: {{ _eq: "{}" }} }}, limit: 2) {{
            _docID command_id requester_did requester_peer_id node_did agent_did session_id session_doc_id intent_json
        }} }}"#,
        escape_graphql_string(&command.command_id),
    )).await?;
    let rows = response["data"]["AgentSessionGoalControl"]
        .as_array()
        .context("goal command observation omitted its records")?;
    anyhow::ensure!(rows.len() <= 1, "goal command identity is ambiguous");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let stored: SessionGoalControl = serde_json::from_str(
        row["intent_json"]
            .as_str()
            .context("retained goal command is unavailable")?,
    )?;
    anyhow::ensure!(
        stored.computed_digest()? == command.computed_digest()?
            && row["command_id"] == command.command_id
            && row["requester_did"] == command.requester_did
            && row["node_did"] == command.node_did
            && row["agent_did"] == command.agent_did
            && row["session_id"] == command.session_id
            && row["session_doc_id"] == serde_json::to_value(&command.session_doc_id)?
            && row["requester_peer_id"] == serde_json::to_value(&command.requester_peer_id)?,
        "goal command identity is bound to different signed input"
    );
    Ok(Some(row.clone()))
}

async fn verified_receipt(
    row: &serde_json::Value,
    signer: &dyn NodeIdentity,
    command: &SessionGoalControl,
) -> Result<Option<SessionGoalControlReceipt>> {
    let receipt = row["receipt_json"]
        .as_str()
        .and_then(|raw| serde_json::from_str::<SessionGoalControlReceipt>(raw).ok());
    if let Some(receipt) = receipt {
        if receipt.validate_for(command).is_ok()
            && signer
                .verify(
                    &command.agent_did,
                    &receipt.signing_payload()?,
                    &receipt.signature,
                )
                .await
                .unwrap_or(false)
        {
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

async fn receipt_row(
    access: &ConfigAccess,
    command_doc_id: &str,
) -> Result<Option<serde_json::Value>> {
    let response = access.execute(&format!(
        r#"{{ AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}, limit: 2) {{ receipt_json }} }}"#,
        escape_graphql_string(command_doc_id),
    )).await?;
    let rows = response["data"]["AgentSessionGoalReceipt"]
        .as_array()
        .context("goal receipt observation omitted its records")?;
    anyhow::ensure!(rows.len() <= 1, "goal receipt identity is ambiguous");
    Ok(rows.first().cloned())
}

/// Missing or unverified receipts remain unconfirmed. This read never creates
/// a command, refreshes its signature, or infers application from current Goal state.
pub async fn recover_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    command: &SessionGoalControl,
) -> Result<Option<SessionGoalControlReceipt>> {
    validate_prepared(signer, command).await?;
    match retained_control(access, command).await? {
        Some(row) => match receipt_row(access, &{
            let document = document_id(&row)?;
            grant_command_relationships(access, command, &document).await?;
            document
        })
        .await?
        {
            Some(receipt) => verified_receipt(&receipt, signer, command).await,
            None => Ok(None),
        },
        None => Ok(None),
    }
}

/// Resumption publishes only the exact retained signature and first request.
/// A committed command is reused even after its issuance window has elapsed;
/// the runtime's existing receipt and authority owners decide its outcome.
pub async fn submit_prepared_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    command: &SessionGoalControl,
) -> Result<SessionGoalControlReceipt> {
    validate_prepared(signer, command).await?;
    let document = if let Some(row) = retained_control(access, command).await? {
        document_id(&row)?
    } else {
        match publish(access, command).await {
            Ok(document) => document,
            Err(error) => match retained_control(access, command).await? {
                Some(row) => document_id(&row)?,
                None => return Err(error),
            },
        }
    };
    grant_command_relationships(access, command, &document).await?;
    if command.requester_did != command.agent_did
        && !(matches!(
            command.operation,
            gents_protocol::goal_control::GoalOperation::Create { .. }
        ) && command.native_evidence.is_some())
    {
        if let Some(row) = receipt_row(access, &document).await? {
            if let Some(receipt) = verified_receipt(&row, signer, command).await? {
                return Ok(receipt);
            }
        }
        anyhow::bail!("Goal control was saved but is blocked until the runtime can atomically recheck the requester's access. It remains pending; retry after that capability is available.");
    }
    let query = format!(
        r#"{{ AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(&document)
    );
    let answered = async {
        loop {
            let response = access.execute(&query).await?;
            let rows = response["data"]["AgentSessionGoalReceipt"]
                .as_array()
                .context("goal receipt observation omitted its records")?;
            anyhow::ensure!(rows.len() <= 1, "goal receipt identity is ambiguous");
            if let Some(row) = rows.first() {
                if let Some(receipt) = verified_receipt(row, signer, command).await? {
                    return Ok::<_, anyhow::Error>(receipt);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), answered).await
        .with_context(|| format!("Goal control {} was saved and is waiting for the runtime; its outcome is not yet confirmed.", command.command_id))?
}

pub(super) async fn grant_command_relationships(
    access: &ConfigAccess,
    command: &SessionGoalControl,
    document: &str,
) -> Result<()> {
    let session = command
        .session_doc_id
        .as_deref()
        .context("goal command has no session document")?;
    access
        .add_resource_relationship(
            "AgentSessionGoalControl",
            document,
            "parent",
            "sessions",
            session,
        )
        .await?;
    access
        .add_actor_relationship(
            "AgentSessionGoalControl",
            document,
            "working_agent",
            &::identity::Did::new(&command.agent_did)?,
        )
        .await?;
    Ok(())
}

fn document_id(row: &serde_json::Value) -> Result<String> {
    row["_docID"]
        .as_str()
        .map(str::to_owned)
        .context("retained goal command has no document identity")
}

pub(super) async fn publish(access: &ConfigAccess, command: &SessionGoalControl) -> Result<String> {
    let session = command
        .session_doc_id
        .as_deref()
        .context("goal command has no session document")?;
    let input = serde_json::json!({
        "command_id": command.command_id,
        "requester_did": command.requester_did,
        "requester_peer_id": command.requester_peer_id,
        "node_did": command.node_did,
        "agent_did": command.agent_did,
        "session_id": command.session_id,
        "session_doc_id": session,
        "intent_json": serde_json::to_string(command)?,
    });
    let response = access.transact_write("goal.submit_session_control", |txn| {
        let input = input.clone();
        Box::pin(async move {
            txn.execute_client_signed_create(
                "mutation($input: AgentSessionGoalControlMutationInputArg!) { create_AgentSessionGoalControl(input: $input) { _docID } }",
                &serde_json::json!({"input": input}),
                gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL,
            ).await.map_err(anyhow::Error::from)
        })
    }).await?;
    Ok(crate::store::graphql::created_doc_id(
        &response,
        "AgentSessionGoalControl",
    )?)
}
