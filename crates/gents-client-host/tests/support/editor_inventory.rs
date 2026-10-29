use super::{editor_review, fake_llm, Attachment, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::{graphql::escape_graphql_string, ConfigAccess};
use gents_protocol::file_image_hydration::FileImageDemand;
use gents_protocol::file_inventory::{FileInventoryObservation, FileInventoryReadAction};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde_json::{json, Value};
use std::path::Path;

pub const COUNT: usize = 65;
pub const PROMPT: &str = "Update every recorded inventory fixture file.";
pub const REPLY: &str = "The recorded inventory files are ready for review.";
pub const NEXT_PROMPT: &str = "Add the next recorded inventory file.";
pub const NEXT_REPLY: &str = "The additional recorded file is ready for review.";
pub const REPEAT_PROMPT: &str = "Edit the same recorded file twice in sequence.";
pub const REPEAT_REPLY: &str = "The sequential recorded changes are ready for review.";
pub const FOLLOW_PROMPT: &str = "Continue without changing any files.";
pub const FOLLOW_REPLY: &str = "The follow-up completed without changing files.";
pub const PARTIAL_PROMPT: &str = "Record one edit alongside an unobserved shell action.";
pub const PARTIAL_REPLY: &str = "The partial-coverage file change is ready.";
const READ_CALL: &str = "inventory-repeat-read";
const REPEAT_CALLS: [&str; 2] = ["inventory-repeat-first", "inventory-repeat-last"];
const UNCHANGED_CALL: &str = "inventory-repeat-unchanged";
const FOLLOW_CALL: &str = "inventory-other-request-unobserved";
const PARTIAL_UNKNOWN_CALL: &str = "inventory-same-request-unobserved";
const PARTIAL_WRITE_CALL: &str = "inventory-partial-write";

fn file(index: usize) -> String {
    match index {
        0 => "inventory/first/shared.txt".to_owned(),
        64 => "inventory/second/shared.txt".to_owned(),
        _ => format!("inventory/file-{index:03}.txt"),
    }
}
fn before(index: usize) -> String {
    format!("Original recorded file {index}\r\nretained original tail {index}\n")
}
fn after(index: usize) -> String {
    format!("Updated recorded file {index}\r\nretained updated tail {index}\n")
}
fn later(index: usize) -> String {
    format!("Later working copy {index} must remain unchanged.\n")
}
fn call_id(index: usize) -> String {
    format!("inventory-write-{index}")
}

pub async fn prepare(root: &Path) -> Result<()> {
    tokio::fs::create_dir_all(root.join("inventory")).await?;
    let mut files = Vec::new();
    for index in 0..=COUNT + 2 {
        let path = root.join(file(index));
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, before(index)).await?;
        files.push(json!({
            "file":file(index), "before":before(index), "after":after(index), "later":later(index),
            "beforeMarker":format!("Original recorded file {index}"),
            "afterMarker":format!("Updated recorded file {index}"),
        }));
    }
    tokio::fs::write(
        root.join("editor-inventory-fixture.json"),
        serde_json::to_vec(&json!({
            "count":COUNT, "prompt":PROMPT, "reply":REPLY,
            "nextPrompt":NEXT_PROMPT, "nextReply":NEXT_REPLY, "files":files,
            "repeatPrompt":REPEAT_PROMPT, "repeatReply":REPEAT_REPLY,
            "followPrompt":FOLLOW_PROMPT, "followReply":FOLLOW_REPLY,
            "partialPrompt":PARTIAL_PROMPT, "partialReply":PARTIAL_REPLY,
        }))?,
    )
    .await?;
    Ok(())
}

pub fn reply(request: &Value) -> Option<ChatAction> {
    let messages = request["messages"].as_array()?;
    let prompt = messages.iter().rev().find_map(|message| {
        if message["role"] != "user" {
            return None;
        }
        match &message["content"] {
            Value::String(text) => Some(text.as_str()),
            Value::Array(parts) => parts.iter().find_map(|part| part["text"].as_str()),
            _ => None,
        }
    })?;
    if prompt == FOLLOW_PROMPT || prompt == PARTIAL_PROMPT {
        let call = if prompt == FOLLOW_PROMPT {
            FOLLOW_CALL
        } else {
            PARTIAL_UNKNOWN_CALL
        };
        if !messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == call)
        {
            return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                call, "bash_unrestricted",
                &json!({"command":"/bin/sh", "args":["-c", "printf 'Unobserved inventory action\\n'"], "timeout_secs":10}).to_string(),
            )));
        }
        if prompt == PARTIAL_PROMPT
            && !messages.iter().any(|message| {
                message["role"] == "tool" && message["tool_call_id"] == PARTIAL_WRITE_CALL
            })
        {
            return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                PARTIAL_WRITE_CALL, "write_file",
                &json!({"path":file(COUNT + 2), "content":after(COUNT + 2), "overwrite":true, "raw_json":true}).to_string(),
            )));
        }
        return Some(ChatAction::Sse(fake_llm::completion_text_sse(
            if prompt == FOLLOW_PROMPT {
                FOLLOW_REPLY
            } else {
                PARTIAL_REPLY
            },
        )));
    }
    if prompt == REPEAT_PROMPT {
        if !messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == READ_CALL)
        {
            return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                READ_CALL,
                "read_file",
                &json!({"path":file(COUNT + 1), "raw_json":true}).to_string(),
            )));
        }
        for (index, call) in REPEAT_CALLS.iter().enumerate() {
            if !messages
                .iter()
                .any(|message| message["role"] == "tool" && message["tool_call_id"] == *call)
            {
                let content = if index == 0 {
                    format!("Intermediate recorded version {}\n", COUNT + 1)
                } else {
                    after(COUNT + 1)
                };
                return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                    call,
                    "write_file",
                    &json!({"path":file(COUNT + 1), "content":content,
                        "overwrite":true, "raw_json":true})
                    .to_string(),
                )));
            }
        }
        if !messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == UNCHANGED_CALL)
        {
            return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                UNCHANGED_CALL, "write_file",
                &json!({"path":file(COUNT + 1), "content":after(COUNT + 1), "overwrite":true, "raw_json":true}).to_string(),
            )));
        }
        return Some(ChatAction::Sse(fake_llm::completion_text_sse(REPEAT_REPLY)));
    }
    let (indices, response) = match prompt {
        PROMPT => (0..COUNT, REPLY),
        NEXT_PROMPT => (COUNT..COUNT + 1, NEXT_REPLY),
        _ => return None,
    };
    if indices.clone().all(|index| {
        messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == call_id(index))
    }) {
        return Some(ChatAction::Sse(fake_llm::completion_text_sse(response)));
    }
    let calls: Vec<_> = indices.enumerate().map(|(slot, index)| json!({
        "index":slot, "id":call_id(index), "function": {
            "name":"write_file", "arguments":json!({
                "path":file(index), "content":after(index), "overwrite":true, "raw_json":true,
            }).to_string(),
        },
    })).collect();
    Some(ChatAction::Sse(format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"tool_calls":calls}, "finish_reason":null}], "usage":null}),
        json!({"choices":[{"delta":{"content":null}, "finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":16,"completion_tokens":4,"total_tokens":20}}),
    )))
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
    verifier: &dyn gents::NodeIdentity,
) -> Result<Value> {
    let ui: Value =
        serde_json::from_slice(&tokio::fs::read(root.join("editor-inventory-ui.json")).await?)?;
    for field in [
        "pagedAllFiles",
        "restoredSelectedDiff",
        "newActivityVisible",
        "restoredAfterNewActivity",
        "workingCopyPreserved",
        "repeatedNetComparison",
        "settledCardUnpinned",
        "repeatedDiffRestored",
        "unknownOtherRequestPreservesTotals",
        "unknownSameRequestHidesTotals",
        "partialSurvivesReload",
    ] {
        ensure!(
            ui[field] == true,
            "installed inventory evidence is missing {field}"
        );
    }
    let selected = usize::try_from(
        ui["selectedIndex"]
            .as_u64()
            .context("selected fixture file")?,
    )?;
    ensure!(
        selected < COUNT,
        "selected file is outside the initial inventory"
    );
    let query = format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{ {} }} }}"#,
        escape_graphql_string(node),
        escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS
    );
    let result = access.execute(&query).await?;
    let requests: Vec<AgentRequestRow> =
        serde_json::from_value(result["data"]["AgentRequest"].clone())?;
    let mut tools = RapidHashMap::new();
    let mut verified_read = false;
    let mut unchanged_tool = None;
    let mut unobserved = RapidHashSet::new();
    for (prompt, expected) in [
        (PROMPT, COUNT),
        (NEXT_PROMPT, 1),
        (REPEAT_PROMPT, 4),
        (FOLLOW_PROMPT, 1),
        (PARTIAL_PROMPT, 2),
    ] {
        let matching: Vec<_> = requests
            .iter()
            .filter(|request| request.content.as_deref() == Some(prompt))
            .collect();
        ensure!(
            matching.len() == 1,
            "inventory input was not admitted exactly once"
        );
        let request = matching[0];
        gents::verify_request_receipt_signature(request)?;
        ensure!(
            request.requester_did.as_deref() == Some(author)
                && request.lifecycle_state == Some(RequestLifecycleState::Completed),
            "inventory input did not complete under its enrolled author"
        );
        let request_doc = request
            .doc_id
            .as_deref()
            .context("physical inventory request")?;
        let calls = access.execute(&format!(r#"{{ AgentToolCall(filter: {{request_doc_id: {{_eq: "{}"}}}}) {{ _docID tool_call_id lifecycle_state }} }}"#,
            escape_graphql_string(request_doc))).await?;
        let calls = calls["data"]["AgentToolCall"]
            .as_array()
            .context("physical inventory tools")?;
        ensure!(
            calls.len() == expected,
            "expected {expected} real tool calls; found {}",
            calls.len()
        );
        for call in calls {
            ensure!(
                call["lifecycle_state"] == "completed",
                "inventory mutation did not complete"
            );
            let logical = field(call, "tool_call_id")?;
            if logical == READ_CALL {
                ensure!(
                    prompt == REPEAT_PROMPT && !verified_read,
                    "read witness scope or identity changed"
                );
                let physical = escape_graphql_string(field(call, "_docID")?);
                let rows = access.execute(&format!(
                    "{{ ToolFileInventory(filter: {{tool_call_doc_id: {{_eq: \"{physical}\"}}}}) {{request_doc_id verified_unchanged observation_doc_id changes}} ToolFileChange(filter: {{tool_call_doc_id: {{_eq: \"{physical}\"}}}}) {{_docID}} }}"
                )).await?;
                let witness = rows["data"]["ToolFileInventory"]
                    .as_array()
                    .context("read witness")?;
                ensure!(
                    witness.len() == 1
                        && witness[0]["request_doc_id"] == request_doc
                        && witness[0]["verified_unchanged"] == true
                        && witness[0]["observation_doc_id"].is_null()
                        && witness[0]["changes"].is_null()
                        && rows["data"]["ToolFileChange"]
                            .as_array()
                            .is_some_and(Vec::is_empty),
                    "a read must retain one byte-free witness without an image"
                );
                verified_read = true;
                continue;
            }
            if logical == UNCHANGED_CALL {
                ensure!(
                    prompt == REPEAT_PROMPT && unchanged_tool.is_none(),
                    "unchanged write scope or identity changed"
                );
                unchanged_tool = Some(field(call, "_docID")?.to_owned());
            }
            if logical == FOLLOW_CALL || logical == PARTIAL_UNKNOWN_CALL {
                ensure!(
                    prompt
                        == if logical == FOLLOW_CALL {
                            FOLLOW_PROMPT
                        } else {
                            PARTIAL_PROMPT
                        },
                    "unobserved call changed requests"
                );
                let physical = field(call, "_docID")?;
                let escaped = escape_graphql_string(physical);
                let rows = access.execute(&format!("{{ ToolFileInventory(filter: {{tool_call_doc_id: {{_eq: \"{escaped}\"}}}}) {{_docID}} ToolFileChange(filter: {{tool_call_doc_id: {{_eq: \"{escaped}\"}}}}) {{_docID}} }}")).await?;
                ensure!(
                    ["ToolFileInventory", "ToolFileChange"]
                        .iter()
                        .all(|collection| rows["data"][*collection]
                            .as_array()
                            .is_some_and(Vec::is_empty))
                        && unobserved.insert(physical.to_owned()),
                    "unobserved execution must not acquire a fabricated no-change witness or image"
                );
                continue;
            }
            let index: usize = if REPEAT_CALLS.contains(&logical) || logical == UNCHANGED_CALL {
                COUNT + 1
            } else if logical == PARTIAL_WRITE_CALL {
                COUNT + 2
            } else {
                logical
                    .strip_prefix("inventory-write-")
                    .context("inventory tool call")?
                    .parse()?
            };
            ensure!(
                tools
                    .insert(field(call, "_docID")?.to_owned(), index)
                    .is_none(),
                "physical mutation was duplicated"
            );
        }
    }
    ensure!(
        verified_read && unchanged_tool.is_some() && unobserved.len() == 2,
        "the mixed requests omitted an unchanged or unobserved operation"
    );
    ensure!(
        tools.len() == COUNT + 5,
        "new activity lost an earlier mutation"
    );
    let mut received = RapidHashSet::new();
    let mut cursor = None;
    let mut pages = 0;
    let mut repeated_positions = RapidHashSet::new();
    loop {
        let requested = cursor.clone();
        let value = client
            .call(
                *id,
                "desktop_session_file_inventory",
                json!({
                    "nodeDid":node, "sessionId":session,
                    "action":FileInventoryReadAction::Read { cursor },
                }),
            )
            .await;
        *id += 1;
        let observation: FileInventoryObservation = serde_json::from_value(value)?;
        // The refusal belongs to the active attempt; a separately validated
        // completed previous snapshot remains readable through its own cursor.
        let expected_entries = u64::try_from(COUNT + 5)?;
        let retained = [&observation.current, &observation.previous]
            .into_iter()
            .flatten()
            .map(|snapshot| {
                (
                    snapshot.complete,
                    snapshot.received_entries,
                    snapshot.known_inventory,
                    snapshot.binding.coverage.clone(),
                )
            })
            .collect::<Vec<_>>();
        let snapshot = observation
            .current
            .into_iter()
            .chain(observation.previous)
            .find(|snapshot| {
                snapshot.complete
                    && !snapshot.known_inventory
                    && snapshot.received_entries == expected_entries
                    && requested.as_ref().is_none_or(|cursor| {
                        snapshot.attempt_id == cursor.attempt_id
                            && snapshot.binding.digest == cursor.snapshot_digest
                            && snapshot.offset == cursor.offset
                    })
            })
            .with_context(|| format!("the installed client did not retain the complete updated inventory (active refusal: {:?}, retained: {:?})", observation.refusal, retained))?;
        ensure!(
            snapshot.binding.coverage.pending == 0 && snapshot.binding.coverage.unknown == 2,
            "the complete snapshot must retain both unobserved shell actions as unknown coverage"
        );
        for entry in snapshot.entries {
            let index = *tools
                .get(&entry.tool_call_doc_id)
                .context("inventory named a different physical tool")?;
            ensure!(
                entry.scope.node_did == node
                    && entry.scope.requester_did == author
                    && entry.scope.session_id == session,
                "inventory source scope changed"
            );
            ensure!(
                tokio::fs::canonicalize(Path::new(&entry.change.root).join(&entry.change.path))
                    .await?
                    == tokio::fs::canonicalize(root.join(file(index))).await?,
                "inventory path differs from its mutation"
            );
            if unchanged_tool.as_deref() == Some(entry.tool_call_doc_id.as_str()) {
                ensure!(
                    entry.change.before == entry.change.after,
                    "the identical-byte write changed its retained image"
                );
            }
            ensure!(
                received.insert(entry.tool_call_doc_id),
                "inventory page duplicated a physical mutation"
            );
            if index == COUNT + 1 {
                ensure!(
                    entry
                        .request_coverage
                        .as_ref()
                        .is_some_and(|coverage| coverage.entries == 3
                            && coverage.pending == 0
                            && coverage.unknown == 0),
                    "mixed request coverage must include all three captures, with no unknown read"
                );
                let order = entry
                    .order
                    .context("repeated edit omitted canonical chronology")?;
                ensure!(
                    order.entries == 3 && repeated_positions.insert(order.index),
                    "repeated edits have inconsistent or duplicate chronology"
                );
            }
            if index == COUNT + 2 {
                ensure!(entry.request_coverage.as_ref().is_some_and(|coverage| coverage.entries == 1 && coverage.pending == 0 && coverage.unknown == 1), "partial request must retain its own unknown action without inheriting another request's unknown coverage");
            }
        }
        pages += 1;
        cursor = snapshot.next;
        if cursor.is_none() {
            break;
        }
    }
    ensure!(
        pages == 2 && received.len() == COUNT + 5,
        "inventory did not cross its native page boundary"
    );
    ensure!(
        (0..3).all(|position| repeated_positions.contains(&position)),
        "all sequential capture positions must survive paging and reload"
    );
    let receipts = access.execute(&format!(r#"{{ FileImageHydrationRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}) {{
        request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id status status_detail
        observation_doc_id processed_at outcome_signer_did outcome_signature
    }} }}"#, escape_graphql_string(node), escape_graphql_string(session), escape_graphql_string(author))).await?;
    let receipts = receipts["data"]["FileImageHydrationRequest"]
        .as_array()
        .context("image demands")?;
    ensure!(
        receipts.len() == 5,
        "metadata browsing or reload created extra image demands: {}",
        receipts.len()
    );
    let mut opened = RapidHashSet::new();
    for receipt in receipts {
        let physical = field(receipt, "tool_call_doc_id")?;
        let index = *tools
            .get(physical)
            .context("image demand named another tool")?;
        ensure!(
            [selected, COUNT, COUNT + 1].contains(&index) && opened.insert(physical.to_owned()),
            "an unseen image was requested"
        );
        let demand = FileImageDemand {
            inventory_reference: None,
            admission: gents_protocol::session_hydration::HydrationRequest {
                request_key: field(receipt, "request_key")?.into(),
                peer_id: field(receipt, "peer_id")?.into(),
                requester_did: author.into(),
                node_did: node.into(),
                session_id: session.into(),
            },
            request_doc_id: field(receipt, "request_doc_id")?.into(),
            tool_call_doc_id: physical.into(),
        };
        editor_review::verify_model_receipt(verifier, receipt, &demand).await?;
        ensure!(
            receipt["status"] == "served" && receipt["observation_doc_id"].is_string(),
            "the selected image was not delivered"
        );
    }
    for index in 0..=COUNT + 2 {
        let expected = if index == selected || index == COUNT + 1 {
            later(index)
        } else {
            after(index)
        };
        ensure!(
            tokio::fs::read_to_string(root.join(file(index))).await? == expected,
            "recorded review changed a working copy"
        );
    }
    Ok(
        json!({"sessionId":session, "initialCaptures":COUNT, "newCaptures":5, "receivedEntries":received.len(),
        "nativePages":pages, "imageDemands":receipts.len(), "selectedIndex":selected,
        "onlySelectedImagesRequested":true, "signedImageReceiptsVerified":true, "workingCopiesPreserved":true,
        "canonicalRepeatedPositions":repeated_positions.len(), "exactlyOnceInputs":5, "verifiedUnchangedRead":verified_read,
        "verifiedIdenticalWrite":unchanged_tool.is_some(), "unobservedActions":unobserved.len(), "selectedRequestCoverageVerified":true}),
    )
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .with_context(|| format!("missing inventory receipt field {name}"))
}
