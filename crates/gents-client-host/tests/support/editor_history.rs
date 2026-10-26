use super::{fake_llm, read_session, read_session_page, Attachment};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

const INPUT_PREFIX: &str = "History fixture turn ";
const ACTION_CALL_ID: &str = "history-profile-read-file";
const ACTION_CONTENT: &str =
    "content:\nL1: # Project instructions fixture\nL2: Use the current editor workspace.";
/// Pending requests can be redelivered after the watcher's 30-second cooldown
/// and 30-second quiet rescan; observation must outlast both without resubmission.
const TURN_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(90);

#[cfg(feature = "editor-e2e")]
pub struct ProfileEvidence(std::path::PathBuf);

#[cfg(feature = "editor-e2e")]
impl ProfileEvidence {
    pub fn from_env(root: &Path) -> Result<Option<Self>> {
        if std::env::var("GENTS_VSCODE_HISTORY_PROFILE").as_deref() != Ok("1") {
            return Ok(None);
        }
        let extension = std::env::var_os("GENTS_VSCODE_ROOT").context("extension root")?;
        let results = Path::new(&extension).join("test-results");
        std::fs::create_dir_all(&results)?;
        let results = std::fs::canonicalize(results)?;
        let version = std::env::var("GENTS_VSCODE_VERSION").unwrap_or("1.142.0".into());
        let mut parts = vec![version];
        if let Some(remote) = std::env::var_os("GENTS_VSCODE_REMOTE_FIXTURE") {
            let remote: Value = serde_json::from_slice(&std::fs::read(remote)?)?;
            parts.push(remote["kind"].as_str().context("remote kind")?.to_owned());
        }
        let mut artifacts = results.join("native-editor");
        for part in parts {
            let mut components = Path::new(&part).components();
            ensure!(
                matches!(components.next(), Some(std::path::Component::Normal(_)))
                    && components.next().is_none(),
                "editor artifact scope must be a single path component"
            );
            artifacts.push(part);
        }
        artifacts.push("history-profile");
        std::fs::create_dir_all(&artifacts)?;
        let artifacts = std::fs::canonicalize(artifacts)?;
        ensure!(
            artifacts.starts_with(results),
            "editor evidence left test-results"
        );
        std::fs::write(
            root.join("editor-artifacts-path"),
            artifacts.to_string_lossy().as_bytes(),
        )?;
        Ok(Some(Self::begin(artifacts)?))
    }

    fn begin(artifacts: std::path::PathBuf) -> Result<Self> {
        std::fs::write(
            artifacts.join("editor-history-profile.json"),
            serde_json::to_vec_pretty(&json!({
                "complete":false, "nativeComplete":false, "phase":"native-setup-or-seeding",
            }))?,
        )?;
        Ok(Self(artifacts))
    }

    pub fn finish(self, root: &Path, result: &Result<()>) -> Result<()> {
        let path = self.0.join("editor-history-profile.json");
        let mut report: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        report["nativeComplete"] = json!(result.is_ok());
        match result {
            Ok(()) => ensure!(
                report["complete"] == true,
                "editor profile returned without complete UI evidence"
            ),
            Err(error) => {
                report["complete"] = json!(false);
                report["nativeFailure"] = json!(format!("{error:#}"));
                report["phase"] = json!(if root.join("editor.log").exists() {
                    "editor-or-native-verification"
                } else {
                    "native-setup-or-seeding"
                });
                std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
                for name in [
                    "runtime.log",
                    "runtime.err",
                    "client-host.log",
                    "editor.log",
                    "editor-history-seed-progress.json",
                    "editor-history-seed-failure.json",
                ] {
                    let source = root.join(name);
                    if source.exists() {
                        std::fs::copy(source, self.0.join(name))?;
                    }
                }
                let failure = root.join("editor-history-seed-failure.json");
                if failure.exists() {
                    report["seedFailure"] = serde_json::from_slice(&std::fs::read(failure)?)?;
                }
            }
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
        Ok(())
    }
}

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

pub fn reply(request: &Value, options: Options) -> Option<fake_llm::ChatAction> {
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
    if !(1..=options.turns).contains(&turn) {
        return None;
    }
    let action_completed = request["messages"]
        .as_array()?
        .iter()
        .any(|message| message["role"] == "tool" && message["tool_call_id"] == ACTION_CALL_ID);
    let response = if options.profile && turn == 1 && !action_completed {
        fake_llm::tool_call_sse_with_id(ACTION_CALL_ID, "read_file", r#"{"path":"AGENTS.md"}"#)
    } else {
        fake_llm::completion_text_sse(&format!("History reply {turn:02}."))
    };
    Some(fake_llm::ChatAction::Sse(response))
}

#[allow(clippy::too_many_arguments)]
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
        let completed = tokio::time::timeout(TURN_OBSERVATION_TIMEOUT, async {
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
        .with_context(|| format!("history turn {turn} did not complete: {last}"))
        .and_then(|result| result);
        if let Err(error) = completed {
            if options.profile {
                std::fs::write(
                    root.join("editor-history-seed-failure.json"),
                    serde_json::to_vec_pretty(&json!({
                        "turn":turn, "completedTurns":turn - 1, "totalTurns":options.turns,
                        "nodeDid":node, "sessionId":session, "requestId":request,
                        "lastOutput":last, "elapsedSeconds":started.elapsed().as_secs_f64(),
                    }))?,
                )?;
            }
            return Err(error);
        }
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
    let mut action = None;
    let action_request = options
        .profile
        .then(|| {
            rows.iter()
                .find(|row| row.content.as_deref() == Some("History fixture turn 01"))
                .context("profile action request")
        })
        .transpose()?;
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
            if options.profile && kind == "toolGroup" {
                let tools = item["tools"].as_array().context("profile action tools")?;
                ensure!(
                    tools.len() == 1 && action.is_none(),
                    "profile must contain exactly one native action"
                );
                let tool = &tools[0];
                let presentation = &tool["presentation"];
                ensure!(
                    tool["toolCallId"] == ACTION_CALL_ID
                        && tool["requestId"].as_str()
                            == action_request.map(|row| row.request_id.as_str())
                        && tool["toolName"] == "read_file"
                        && tool["statusKind"] == "success"
                        && tool["reconstruction"]["state"] == "ready"
                        && presentation["kind"] == "fileRead"
                        && presentation["target"] == "AGENTS.md"
                        && presentation["returnedCount"] == 2
                        && presentation["totalCount"] == 2
                        && presentation["truncated"] == false
                        && presentation["body"] == ACTION_CONTENT,
                    "profile action did not preserve its canonical identity and file contents: {tool}"
                );
                ensure!(
                    tool["itemKey"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()),
                    "profile action has no canonical itemKey"
                );
                let doc_id = canonical_action_doc_id(
                    access,
                    node,
                    session,
                    action_request.context("profile action request")?,
                    tool,
                )
                .await?;
                action = Some(json!({
                    "itemKey":tool["itemKey"], "timelineItemKey":key,
                    "toolCallDocId":doc_id, "requestId":tool["requestId"],
                    "toolCallId":tool["toolCallId"], "toolName":tool["toolName"],
                    "statusKind":tool["statusKind"], "fileName":"AGENTS.md",
                    "content":presentation["body"],
                }));
            }
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
    ensure!(
        !options.profile || action.is_some(),
        "history profile did not retain its native action"
    );
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "completedRequests":rows.len(), "verifiedSignatures":rows.len(),
        "seededTurns":options.turns, "seedSeconds":seed_seconds,
        "latestPage":snapshot["timelinePage"],
        "latestItems":snapshot["timelineItems"].as_array().map(Vec::len),
        "timelineItems":keys.len(), "timelineItemKeys":keys,
        "timelineKindCounts":kinds, "timelinePages":pages, "profile":options.profile,
        "action":action,
    }))
}

async fn canonical_action_doc_id(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    request: &gents_protocol::row::AgentRequestRow,
    rendered: &Value,
) -> Result<String> {
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("profile request document")?;
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}},
            request_doc_id: {{_eq: "{}"}}, tool_call_id: {{_eq: "{}"}}}}, limit: 2) {{
            _docID node_did session_id request_id request_doc_id requester_did
            tool_call_key tool_call_id tool_name lifecycle_state
        }} }}"#,
            gents::graphql::escape_graphql_string(node),
            gents::graphql::escape_graphql_string(session),
            gents::graphql::escape_graphql_string(request_doc_id),
            gents::graphql::escape_graphql_string(ACTION_CALL_ID),
        ))
        .await?;
    let rows: Vec<gents_protocol::row::AgentToolCallRow> =
        serde_json::from_value(response["data"]["AgentToolCall"].clone())?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("profile action must resolve to exactly one canonical tool record");
    };
    ensure!(
        row.node_did.as_deref() == Some(node)
            && row.session_id.as_deref() == Some(session)
            && row.request_doc_id.as_deref() == Some(request_doc_id)
            && row.request_id.as_deref() == Some(request.request_id.as_str())
            && row.requester_did == request.requester_did
            && row.tool_call_id.as_deref() == Some(ACTION_CALL_ID)
            && row.tool_name.as_deref() == Some("read_file")
            && row.lifecycle_state.as_deref() == Some("completed")
            && rendered["itemKey"].as_str() == Some(row.tool_call_key.as_str()),
        "profile action crossed its canonical request, tool, or renderer identity: {row:?}"
    );
    row.doc_id
        .clone()
        .filter(|value| !value.is_empty())
        .context("profile action has no canonical document identity")
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

#[allow(clippy::too_many_arguments)]
pub async fn verify_profile(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    node: &str,
    session: &str,
    author: &str,
    options: Options,
    command_output: bool,
) -> Result<Value> {
    let rows = verified_requests(
        access,
        node,
        session,
        author,
        options.turns + 2 + usize::from(command_output),
    )
    .await?;
    let activity = if command_output {
        let commands = rows
            .iter()
            .filter(|row| {
                row.content.as_deref() == Some(super::editor_command_output::ACTIVITY_PROMPT)
            })
            .collect::<Vec<_>>();
        let [request] = commands.as_slice() else {
            anyhow::bail!("history profile must admit exactly one command-output request");
        };
        super::editor_command_output::verify(access, node, session, request).await?
    } else {
        Value::Null
    };
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
        "commandOutput":activity,
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
    assert!(reply(&request, default).is_none());
    let Some(fake_llm::ChatAction::Sse(response)) = reply(&request, long) else {
        panic!("history completion");
    };
    assert_eq!(
        response,
        fake_llm::completion_text_sse("History reply 1000.")
    );
}

#[test]
fn history_profile_calls_read_file_until_its_exact_result_then_completes() {
    let profile = Options::parse(Some("120"), Some("1")).unwrap();
    let ordinary = Options::parse(None, None).unwrap();
    let mut request = json!({"messages":[{"role":"user","content":"History fixture turn 01"}]});
    let response = |request: &Value, options| {
        let Some(fake_llm::ChatAction::Sse(response)) = reply(request, options) else {
            panic!("history response");
        };
        response
    };
    let completion = fake_llm::completion_text_sse("History reply 01.");
    let call =
        fake_llm::tool_call_sse_with_id(ACTION_CALL_ID, "read_file", r#"{"path":"AGENTS.md"}"#);
    assert_eq!(response(&request, ordinary), completion);
    assert_eq!(response(&request, profile), call);
    request["messages"].as_array_mut().unwrap().push(json!({
        "role":"tool", "tool_call_id":"another-call", "content":ACTION_CONTENT,
    }));
    assert_eq!(response(&request, profile), call);
    request["messages"].as_array_mut().unwrap().push(json!({
        "role":"tool", "tool_call_id":ACTION_CALL_ID, "content":ACTION_CONTENT,
    }));
    assert_eq!(response(&request, profile), completion);
    request["messages"].as_array_mut().unwrap().push(json!({
        "role":"user", "content":[{"type":"text","text":"History fixture turn 02"}],
    }));
    assert_eq!(
        response(&request, profile),
        fake_llm::completion_text_sse("History reply 02.")
    );
}

#[cfg(feature = "editor-e2e")]
#[test]
fn history_profile_replaces_stale_success_before_seeding_and_preserves_failure() -> Result<()> {
    let artifacts = tempfile::tempdir()?;
    let root = tempfile::tempdir()?;
    let report = artifacts.path().join("editor-history-profile.json");
    std::fs::write(&report, r#"{"complete":true,"seededTurns":120}"#)?;
    let evidence = ProfileEvidence::begin(artifacts.path().to_owned())?;
    let started: Value = serde_json::from_slice(&std::fs::read(&report)?)?;
    assert_eq!(started["complete"], false);
    assert!(started.get("seededTurns").is_none());
    let failure = json!({
        "turn":277, "completedTurns":276, "totalTurns":1000,
        "requestId":"stalled-request", "lastOutput":{"output":{"kind":"loading"}},
    });
    std::fs::write(
        root.path().join("editor-history-seed-failure.json"),
        serde_json::to_vec(&failure)?,
    )?;
    std::fs::write(root.path().join("runtime.err"), "native stall evidence")?;
    evidence.finish(root.path(), &Err(anyhow::anyhow!("seed deadline elapsed")))?;
    let failed: Value = serde_json::from_slice(&std::fs::read(report)?)?;
    assert_eq!(failed["complete"], false);
    assert_eq!(failed["nativeComplete"], false);
    assert_eq!(failed["phase"], "native-setup-or-seeding");
    assert_eq!(failed["seedFailure"], failure);
    assert_eq!(failed["nativeFailure"], "seed deadline elapsed");
    assert_eq!(
        std::fs::read_to_string(artifacts.path().join("runtime.err"))?,
        "native stall evidence"
    );
    Ok(())
}
