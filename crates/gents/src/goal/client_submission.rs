use gents_protocol::goal_control::{SessionGoalControl, SessionGoalControlReceipt};

use super::*;
use crate::config_client::ConfigAccess;
use crate::identity::NodeIdentity;

#[cfg(test)]
mod tests;

/// Publishes one author-signed intent. The runtime receipt confirms the
/// transaction; observing its replicated Goal is a separate read.
pub async fn submit_session_goal_control(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    mut command: SessionGoalControl,
) -> Result<SessionGoalControlReceipt> {
    if command.requester_did != signer.did() {
        return Err(GoalError::GoalControlSignerDiffersAuthor);
    }
    command.signature = signer.sign(&command.signing_payload()?).await?;
    command.validate_shape()?;
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
    let doc_id = crate::graphql::created_doc_id(&response, "AgentSessionGoalControl")?;
    let query = format!(
        r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(&doc_id)
    );
    let answered = async {
        loop {
            let response = access.execute(&query).await?;
            let receipt = response["data"]["AgentSessionGoalControl"][0]["receipt_json"]
                .as_str()
                .and_then(|raw| serde_json::from_str::<SessionGoalControlReceipt>(raw).ok());
            if let Some(receipt) = receipt {
                if receipt.validate_for(&command).is_ok()
                    && signer
                        .verify(
                            &command.node_did,
                            &receipt.signing_payload()?,
                            &receipt.signature,
                        )
                        .await
                        .unwrap_or(false)
                {
                    return Ok::<_, GoalError>(receipt);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), answered)
        .await
        .map_err(|_| GoalError::GoalControlWasSavedWaitingRuntime {
            command_id: command.command_id.to_string(),
        })?
}
