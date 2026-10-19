use super::{read_session, Attachment};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use std::time::Duration;

pub const TURNS: usize = 45;
const INPUT_PREFIX: &str = "History fixture turn ";

pub fn reply(request: &Value) -> Option<String> {
    let message = request["messages"]
        .as_array()?
        .iter()
        .rev()
        .find(|message| message["role"] == "user")?;
    let sequence = |text: &str| text.strip_prefix(INPUT_PREFIX)?.parse::<usize>().ok();
    let turn = match &message["content"] {
        Value::String(text) => sequence(text),
        Value::Array(parts) => parts
            .iter()
            .find_map(|part| part["text"].as_str().and_then(sequence)),
        _ => None,
    }?;
    (1..=TURNS)
        .contains(&turn)
        .then(|| format!("History reply {turn:02}."))
}

pub async fn populate(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    node: &str,
    session: &str,
    agent: &str,
    author: &str,
) -> Result<Value> {
    let started = std::time::Instant::now();
    for turn in 1..=TURNS {
        let receipt = client
            .call(
                *id,
                "desktop_chat_send",
                json!({"request":{
                    "nodeDid":node, "sessionId":session, "agentId":agent,
                    "content":format!("{INPUT_PREFIX}{turn:02}"), "delivery":"queue"
                }}),
            )
            .await;
        *id += 1;
        let request = receipt["requestId"].as_str().context("history request")?;
        ensure!(receipt["sessionId"] == session, "history changed sessions");
        let mut last = Value::Null;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let output = client
                    .call(
                        *id,
                        "desktop_request_output",
                        json!({
                            "nodeDid":node, "sessionId":session, "requestId":request,
                        }),
                    )
                    .await;
                *id += 1;
                ensure!(
                    output["nodeDid"] == node
                        && output["sessionId"] == session
                        && output["requestId"] == request,
                    "history output crossed the exact request scope"
                );
                match output["output"]["kind"].as_str() {
                    Some("terminalMessage") => {
                        ensure!(
                            output["output"]["content"] == format!("History reply {turn:02}."),
                            "unexpected history output: {output}"
                        );
                        return Ok::<_, anyhow::Error>(());
                    }
                    Some("loading") => last = output,
                    _ => anyhow::bail!("history request did not complete normally: {output}"),
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .with_context(|| format!("history turn {turn} did not complete: {last}"))??;
    }
    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    ensure!(rows.len() == TURNS + 1, "unexpected history request count");
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.admission_signer_did.as_deref() == Some(author)
                && row.requester_did.as_deref() == Some(author)
                && row.lifecycle_state == Some(RequestLifecycleState::Completed),
            "history must contain only completed requests from the enrolled client"
        );
    }
    let snapshot = read_session(client, id, node, session).await?;
    ensure!(
        snapshot["timelinePage"]["hasOlder"] == true,
        "history did not cross the native transcript page boundary: {snapshot}"
    );
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "completedRequests":rows.len(), "verifiedSignatures":rows.len(),
        "seededTurns":TURNS, "seedSeconds":started.elapsed().as_secs_f64(),
        "latestPage":snapshot["timelinePage"],
        "latestItems":snapshot["timelineItems"].as_array().map(Vec::len)
    }))
}
