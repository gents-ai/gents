use anyhow::{Context, Result};
use gents_protocol::process_control::{ProcessStop, ProcessStopReceipt};

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::identity::NodeIdentity;

pub async fn prepare_process_stop(
    signer: &dyn NodeIdentity,
    mut command: ProcessStop,
) -> Result<ProcessStop> {
    anyhow::ensure!(
        command.requester_did == signer.did(),
        "process stop signer differs from its author"
    );
    command.signature = signer.sign(&command.signing_payload()).await?;
    command.validate_shape()?;
    Ok(command)
}

/// Reuses the exact signed intent on retry; only a verified final receipt confirms the result.
pub async fn submit_prepared_process_stop(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    command: &ProcessStop,
) -> Result<ProcessStopReceipt> {
    command.validate_shape()?;
    anyhow::ensure!(
        command.requester_did == signer.did()
            && signer
                .verify(
                    &command.requester_did,
                    &command.signing_payload(),
                    &command.signature
                )
                .await?,
        "process stop author signature is invalid"
    );
    let lookup = format!(
        r#"{{ AgentSessionProcessStop(filter: {{ command_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID command_id requester_did requester_peer_id node_did session_id intent_json receipt_json }} }}"#,
        escape_graphql_string(&command.command_id)
    );
    let retained = |response: serde_json::Value| -> Result<Option<serde_json::Value>> {
        let rows = response["data"]["AgentSessionProcessStop"]
            .as_array()
            .context("process stop lookup omitted records")?;
        anyhow::ensure!(
            rows.len() <= 1,
            "process stop command identity is ambiguous"
        );
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let stored: ProcessStop = serde_json::from_str(
            row["intent_json"]
                .as_str()
                .context("process stop intent is missing")?,
        )?;
        anyhow::ensure!(
            stored == *command
                && row["command_id"] == command.command_id
                && row["requester_did"] == command.requester_did
                && row["requester_peer_id"] == serde_json::to_value(&command.requester_peer_id)?
                && row["node_did"] == command.node_did
                && row["session_id"] == command.session_id,
            "process stop identity is bound to different signed input"
        );
        Ok(Some(row.clone()))
    };
    if retained(access.execute(&lookup).await?)?.is_none() {
        let peer = command
            .requester_peer_id
            .as_deref()
            .map(|value| format!("\"{}\"", escape_graphql_string(value)))
            .unwrap_or_else(|| "null".to_owned());
        let mutation = format!(
            r#"mutation {{ create_AgentSessionProcessStop(input: {{ command_id: "{}", requester_did: "{}", requester_peer_id: {peer}, node_did: "{}", session_id: "{}", intent_json: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&command.command_id),
            escape_graphql_string(&command.requester_did),
            escape_graphql_string(&command.node_did),
            escape_graphql_string(&command.session_id),
            escape_graphql_string(&serde_json::to_string(command)?)
        );
        if let Err(error) = access.write("process_stop.submit", &mutation).await {
            if retained(access.execute(&lookup).await?)?.is_none() {
                return Err(error.into());
            }
        }
    }
    let answered = async {
        loop {
            if let Some(row) = retained(access.execute(&lookup).await?)? {
                if let Some(receipt) = row["receipt_json"]
                    .as_str()
                    .and_then(|raw| serde_json::from_str::<ProcessStopReceipt>(raw).ok())
                {
                    if receipt.validate_for(command).is_ok()
                        && !receipt.outcome.needs_execution()
                        && signer
                            .verify(
                                &command.node_did,
                                &receipt.signing_payload(),
                                &receipt.signature,
                            )
                            .await
                            .unwrap_or(false)
                    {
                        return Ok::<_, anyhow::Error>(receipt);
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), answered).await
        .with_context(|| format!("Process stop {} was saved and is waiting for the runtime; its outcome is not yet confirmed.", command.command_id))?
}
