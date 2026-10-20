use super::{read_session, read_session_page, Attachment};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

const INPUT_PREFIX: &str = "History fixture turn ";
/// Pending requests can be redelivered after the watcher's 30-second cooldown
/// and 30-second quiet rescan; observation must outlast both without resubmission.
const TURN_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy)]
pub struct Options {
    pub turns: usize,
    pub profile: bool,
}

impl Options {
    pub fn from_env() -> Result<Self> {
        Self::parse(
            std::env::var("GENTS_VSCODE_HISTORY_TURNS").ok().as_deref(),
            std::env::var("GENTS_VSCODE_HISTORY_PROFILE")
                .ok()
                .as_deref(),
        )
    }

    fn parse(turns: Option<&str>, profile: Option<&str>) -> Result<Self> {
        ensure!(
            matches!(profile, None | Some("0") | Some("1")),
            "GENTS_VSCODE_HISTORY_PROFILE must be 0 or 1"
        );
        let profile = profile == Some("1");
        let turns = turns
            .unwrap_or(if profile { "1000" } else { "45" })
            .parse::<usize>()
            .context("GENTS_VSCODE_HISTORY_TURNS must be an integer in 45..=5000")?;
        ensure!(
            (45..=5000).contains(&turns),
            "GENTS_VSCODE_HISTORY_TURNS must be in 45..=5000"
        );
        ensure!(
            if profile { turns >= 100 } else { turns == 45 },
            "history profiling requires 100..=5000 turns; without GENTS_VSCODE_HISTORY_PROFILE=1 the ordinary journey requires 45"
        );
        Ok(Self { turns, profile })
    }
}

pub fn reply(request: &Value, turns: usize) -> Option<String> {
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
    (1..=turns)
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
    options: Options,
    root: &Path,
) -> Result<Value> {
    let started = std::time::Instant::now();
    let progress = |completed| -> Result<()> {
        std::fs::write(
            root.join("editor-history-seed-progress.json"),
            serde_json::to_vec_pretty(&json!({
                "completedTurns":completed, "totalTurns":options.turns,
                "elapsedSeconds":started.elapsed().as_secs_f64(),
            }))?,
        )?;
        Ok(())
    };
    progress(0)?;
    for turn in 1..=options.turns {
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
        tokio::time::timeout(TURN_OBSERVATION_TIMEOUT, async {
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
        if turn % 100 == 0 || turn == options.turns {
            progress(turn)?;
        }
    }
    let seed_seconds = started.elapsed().as_secs_f64();
    let rows = verified_requests(access, node, session, author, options.turns + 1).await?;
    let snapshot = read_session(client, id, node, session).await?;
    ensure!(
        snapshot["timelinePage"]["hasOlder"] == true,
        "history did not cross the native transcript page boundary: {snapshot}"
    );
    let mut page = snapshot.clone();
    let mut keys = BTreeSet::new();
    let mut kinds = BTreeMap::<String, usize>::new();
    let mut pages = 0;
    loop {
        pages += 1;
        ensure!(
            pages <= options.turns + 1,
            "history paging did not terminate"
        );
        let items = page["timelineItems"]
            .as_array()
            .context("history page has no timeline items")?;
        ensure!(!items.is_empty(), "history page unexpectedly empty");
        for item in items {
            let key = item["itemKey"].as_str().context("history item key")?;
            ensure!(keys.insert(key.to_owned()), "history paging repeated {key}");
            let kind = item["kind"].as_str().context("history item kind")?;
            *kinds.entry(kind.to_owned()).or_default() += 1;
        }
        if page["timelinePage"]["hasOlder"] == false {
            break;
        }
        let cursor = page["timelinePage"]["oldestItemKey"]
            .as_str()
            .context("history page has no older cursor")?
            .to_owned();
        page = read_session_page(client, id, node, session, Some(&cursor)).await?;
        ensure!(
            page["nodeDid"] == node
                && page["sessionId"] == session
                && page["timelinePage"]["oldestItemKey"] != cursor,
            "history page crossed scope or failed to advance"
        );
    }
    ensure!(
        kinds.get("userMessage") == Some(&(options.turns + 1)),
        "history paging did not retain every authored turn"
    );
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "completedRequests":rows.len(), "verifiedSignatures":rows.len(),
        "seededTurns":options.turns, "seedSeconds":seed_seconds,
        "latestPage":snapshot["timelinePage"],
        "latestItems":snapshot["timelineItems"].as_array().map(Vec::len),
        "timelineItems":keys.len(), "timelineItemKeys":keys,
        "timelineKindCounts":kinds, "timelinePages":pages, "profile":options.profile,
    }))
}

async fn verified_requests(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    author: &str,
    expected: usize,
) -> Result<Vec<gents_protocol::row::AgentRequestRow>> {
    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    ensure!(rows.len() == expected, "unexpected history request count");
    for row in &rows {
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.admission_signer_did.as_deref() == Some(author)
                && row.requester_did.as_deref() == Some(author)
                && row.lifecycle_state == Some(RequestLifecycleState::Completed),
            "history must contain only completed requests from the enrolled client"
        );
    }
    Ok(rows)
}

pub async fn verify_profile(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    node: &str,
    session: &str,
    author: &str,
    options: Options,
) -> Result<Value> {
    let rows = verified_requests(access, node, session, author, options.turns + 2).await?;
    let streamed = rows
        .iter()
        .filter(|row| row.content.as_deref() == Some("History profile streaming request."))
        .collect::<Vec<_>>();
    ensure!(
        streamed.len() == 1,
        "profile must submit exactly one additional streaming request"
    );
    let request = &streamed[0].request_id;
    let output = client
        .call(
            *id,
            "desktop_request_output",
            json!({"nodeDid":node, "sessionId":session, "requestId":request}),
        )
        .await;
    *id += 1;
    ensure!(
        output["nodeDid"] == node
            && output["sessionId"] == session
            && output["requestId"] == *request
            && output["output"]["kind"] == "terminalMessage"
            && output["output"]["content"] == "First part and final part.",
        "profile stream did not preserve its exact canonical terminal output"
    );
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "completedRequests":rows.len(), "verifiedSignatures":rows.len(),
        "streamingRequestId":request, "streamingOutput":output["output"],
    }))
}

#[test]
fn history_options_validate_explicit_qualification_scope() {
    let default = Options::parse(None, None).unwrap();
    assert_eq!(default.turns, 45);
    assert!(!default.profile);
    assert_eq!(Options::parse(None, Some("1")).unwrap().turns, 1000);
    let long = Options::parse(Some("1000"), Some("1")).unwrap();
    assert_eq!(long.turns, 1000);
    assert!(long.profile);
    for value in ["", "-1", "44", "5001", "invalid"] {
        assert!(Options::parse(Some(value), None).is_err());
    }
    assert!(Options::parse(Some("5000"), Some("1")).is_ok());
    assert!(Options::parse(Some("100"), Some("1")).is_ok());
    assert!(Options::parse(Some("99"), Some("1")).is_err());
    assert!(Options::parse(Some("45"), Some("1")).is_err());
    assert!(Options::parse(Some("120"), None).is_err());
    assert!(Options::parse(Some("5000"), Some("0")).is_err());
    assert!(Options::parse(None, Some("true")).is_err());
    let request = json!({"messages":[{"role":"user","content":"History fixture turn 1000"}]});
    assert!(reply(&request, 45).is_none());
    assert_eq!(
        reply(&request, 1000).as_deref(),
        Some("History reply 1000.")
    );
}
