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

fn file(index: usize) -> String {
    format!("inventory/file-{index:03}.txt")
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
    for index in 0..=COUNT {
        tokio::fs::write(root.join(file(index)), before(index)).await?;
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
    for (prompt, expected) in [(PROMPT, COUNT), (NEXT_PROMPT, 1)] {
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
            "expected {expected} real mutations; found {}",
            calls.len()
        );
        for call in calls {
            ensure!(
                call["lifecycle_state"] == "completed",
                "inventory mutation did not complete"
            );
            let logical = field(call, "tool_call_id")?;
            let index: usize = logical
                .strip_prefix("inventory-write-")
                .context("inventory tool call")?
                .parse()?;
            ensure!(
                tools
                    .insert(field(call, "_docID")?.to_owned(), index)
                    .is_none(),
                "physical mutation was duplicated"
            );
        }
    }
    ensure!(
        tools.len() == COUNT + 1,
        "new activity lost an earlier mutation"
    );
    let mut received = RapidHashSet::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
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
        ensure!(
            observation.refusal.is_none(),
            "inventory metadata was refused"
        );
        let snapshot = observation.current.context("complete retained inventory")?;
        ensure!(
            snapshot.complete
                && snapshot.known_inventory
                && snapshot.received_entries == u64::try_from(COUNT + 1)?,
            "the installed client did not retain the complete updated inventory"
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
            ensure!(
                received.insert(entry.tool_call_doc_id),
                "inventory page duplicated a physical mutation"
            );
        }
        pages += 1;
        cursor = snapshot.next;
        if cursor.is_none() {
            break;
        }
    }
    ensure!(
        pages == 2 && received.len() == COUNT + 1,
        "inventory did not cross its native page boundary"
    );
    let receipts = access.execute(&format!(r#"{{ FileImageHydrationRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, requester_did: {{_eq: "{}"}}}}) {{
        request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id status status_detail
        observation_doc_id processed_at outcome_signer_did outcome_signature
    }} }}"#, escape_graphql_string(node), escape_graphql_string(session), escape_graphql_string(author))).await?;
    let receipts = receipts["data"]["FileImageHydrationRequest"]
        .as_array()
        .context("image demands")?;
    ensure!(
        receipts.len() == 2,
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
            [selected, COUNT].contains(&index) && opened.insert(index),
            "an unseen image was requested"
        );
        let demand = FileImageDemand {
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
    for index in 0..=COUNT {
        let expected = if index == selected {
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
        json!({"sessionId":session, "initialCaptures":COUNT, "newCaptures":1, "receivedEntries":received.len(),
        "nativePages":pages, "imageDemands":receipts.len(), "selectedIndex":selected,
        "onlySelectedImagesRequested":true, "signedImageReceiptsVerified":true, "workingCopiesPreserved":true}),
    )
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .with_context(|| format!("missing inventory receipt field {name}"))
}
