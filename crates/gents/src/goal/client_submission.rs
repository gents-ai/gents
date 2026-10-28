use gents_protocol::goal_control::{SessionGoalControl, SessionGoalControlReceipt};

use super::*;
use crate::config_client::ConfigAccess;
use crate::identity::NodeIdentity;

mod provenance;
#[cfg(test)]
mod tests;
pub use provenance::{
    load_applied_opening_goal_id, load_applied_opening_goal_ids,
    load_applied_opening_goal_provenance, OpeningGoalProvenance,
};

/// Publishes one author-signed intent. The runtime receipt confirms the
/// transaction; observing its replicated Goal is a separate read.
pub async fn submit_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    command: SessionGoalControl,
) -> Result<SessionGoalControlReceipt> {
    let command = prepare_session_goal_control(signer, command).await?;
    submit_prepared_session_goal_control(access, signer, &command).await
}

/// Preparation has no database effect. A caller retaining this command must
/// commit it to its existing encrypted journal before publication is allowed.
pub async fn prepare_session_goal_control(
    signer: &dyn NodeIdentity,
    mut command: SessionGoalControl,
) -> Result<SessionGoalControl> {
    anyhow::ensure!(
        command.requester_did == signer.did(),
        "goal control signer differs from its author"
    );
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
            _docID command_id requester_did requester_peer_id node_did session_id intent_json receipt_json
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
            && row["session_id"] == command.session_id
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
                    &command.node_did,
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

/// Missing or unverified receipts remain unconfirmed. This read never creates
/// a command, refreshes its signature, or infers application from current Goal state.
pub async fn recover_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    command: &SessionGoalControl,
) -> Result<Option<SessionGoalControlReceipt>> {
    validate_prepared(signer, command).await?;
    match retained_control(access, command).await? {
        Some(row) => verified_receipt(&row, signer, command).await,
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
    let query = format!(
        r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(&document)
    );
    let answered = async {
        loop {
            let response = access.execute(&query).await?;
            let rows = response["data"]["AgentSessionGoalControl"]
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

fn document_id(row: &serde_json::Value) -> Result<String> {
    row["_docID"]
        .as_str()
        .map(str::to_owned)
        .context("retained goal command has no document identity")
}

async fn publish(access: &ConfigAccess, command: &SessionGoalControl) -> Result<String> {
    let peer = command
        .requester_peer_id
        .as_deref()
        .map(|peer| format!(r#""{}""#, escape_graphql_string(peer)))
        .unwrap_or_else(|| "null".into());
    let response = access
        .write(
            "goal.submit_session_control",
            &format!(
                r#"mutation {{
        create_AgentSessionGoalControl(input: {{
            command_id: "{}", requester_did: "{}", requester_peer_id: {peer},
            node_did: "{}", session_id: "{}", intent_json: "{}"
        }}) {{ _docID }}
    }}"#,
                escape_graphql_string(&command.command_id),
                escape_graphql_string(&command.requester_did),
                escape_graphql_string(&command.node_did),
                escape_graphql_string(&command.session_id),
                escape_graphql_string(&serde_json::to_string(&command)?)
            ),
        )
        .await?;
    Ok(crate::graphql::created_doc_id(
        &response,
        "AgentSessionGoalControl",
    )?)
}
