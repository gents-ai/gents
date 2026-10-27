use super::{fake_llm, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::ConfigAccess;
use gents_protocol::{request_lifecycle::RequestLifecycleState, row::AgentRequestRow};
use serde_json::{json, Value};

pub const PROMPT: &str = "Ask me which worker approach to use.";
pub const EDITED_PROMPT: &str = "Inspect only the parser and report QUESTION_WORKER_EDGE_CASES.";
pub const ANSWER_USED: &str = "Using your edited worker prompt.";
pub const WORKER_REPLY: &str = "QUESTION_WORKER_EDGE_CASES inspected.";

pub fn reply(request: &Value) -> Option<ChatAction> {
    let messages = request["messages"].as_array()?;
    let last_user = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")?;
    let content = match &last_user["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let returned = |id: &str| {
        messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == id)
    };
    if content.contains(EDITED_PROMPT) {
        if !content.contains("Decision on Worker approach:") {
            return Some(ChatAction::Sse(fake_llm::completion_text_sse(WORKER_REPLY)));
        }
        return Some(ChatAction::Sse(if returned("question-worker") {
            fake_llm::completion_text_sse(ANSWER_USED)
        } else {
            fake_llm::tool_call_sse_with_id("question-worker", "agent_new", &json!({"agent":"related-worker-0", "prompt":EDITED_PROMPT, "title":"Inspect parser edge cases"}).to_string())
        }));
    }
    if !content.ends_with(PROMPT) {
        return None;
    }
    Some(ChatAction::Sse(if returned("question-approaches") {
        fake_llm::completion_text_sse("Choose the worker approach below.")
    } else {
        fake_llm::tool_call_sse_with_id("question-approaches", gents::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME, &json!({
            "title":"Worker approach", "question":{
                "version":3, "prompt":"What should the worker inspect?", "allow_free_text":true,
                "options":[
                    {"id":"parser", "label":"Inspect parser", "description":"A focused review with less disruption.", "prompt":"Inspect the parser and propose a focused patch."},
                    {"id":"related", "label":"Inspect related modules", "description":"A wider comparison before choosing a patch.", "prompt":"Compare the parser and related modules."},
                    {"id":"tests", "label":"Review test gaps", "description":"Gather evidence before changing behavior.", "prompt":"Review parser test coverage and report gaps."}
                ]
            }
        }).to_string())
    }))
}

pub async fn verify(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    author: &str,
) -> Result<Value> {
    use gents::graphql::escape_graphql_string as escape;
    let response = access.execute(&format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ {} }} MailboxItem(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}) {{ {} }} }}", escape(node), gents::SIGNED_REQUEST_FIELDS, escape(node), escape(session), gents::mailbox::MAILBOX_FIELDS)).await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let questions: Vec<gents::mailbox::MailboxItem> =
        serde_json::from_value(response["data"]["MailboxItem"].clone())?;
    let [question] = questions.as_slice() else {
        anyhow::bail!("expected one native question");
    };
    ensure!(
        question.status == "acted",
        "the signed answer must consume the native question"
    );
    let parents: Vec<_> = rows
        .iter()
        .filter(|row| row.content.as_deref() == Some(PROMPT))
        .collect();
    let [parent] = parents.as_slice() else {
        anyhow::bail!("expected one asking request");
    };
    ensure!(
        parent.lifecycle_state == Some(RequestLifecycleState::Completed),
        "asking must complete without blocking the owned loop"
    );
    let answers: Vec<_> = rows
        .iter()
        .filter(|row| row.caused_by_source_doc_id.as_deref() == Some(question.doc_id.as_str()))
        .collect();
    let [answer] = answers.as_slice() else {
        anyhow::bail!("expected one correlated answer");
    };
    for row in [*parent, *answer] {
        gents::verify_request_receipt_signature(row)?;
        ensure!(
            row.requester_did.as_deref() == Some(author)
                && row.session_id.as_deref() == Some(session),
            "question scope changed"
        );
    }
    let evidence = answer
        .input
        .as_ref()
        .and_then(|input| input.question_reply.as_ref())
        .context("signed answer evidence")?;
    ensure!(
        evidence.question_version == Some(3)
            && evidence.answer.option_ids == ["parser"]
            && evidence.answer.free_text.as_deref() == Some(EDITED_PROMPT),
        "the native answer must retain the edited prompt and selected choice"
    );
    ensure!(
        answer.doc_id == question.resolved_doc_id
            && answer.lifecycle_state == Some(RequestLifecycleState::Completed),
        "answer must complete exactly once"
    );
    evidence.validate_for(
        &question.doc_id,
        &question.title,
        question.summary.as_deref(),
        question.payload.as_deref().context("question payload")?,
        question.request_id.as_deref(),
        answer.content.as_deref().context("answer text")?,
    )?;
    let workers: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.content
                .as_deref()
                .is_some_and(|text| text.ends_with(EDITED_PROMPT))
                && row
                    .input
                    .as_ref()
                    .is_none_or(|input| input.question_reply.is_none())
        })
        .collect();
    let [worker] = workers.as_slice() else {
        anyhow::bail!("expected one native delegated request");
    };
    gents::verify_request_receipt_signature(worker)?;
    ensure!(
        worker.caused_by_parent_request_doc_id == answer.doc_id
            && worker.caused_by_parent_tool_call_doc_id.is_some()
            && worker.session_id != answer.session_id
            && worker.requester_did.as_deref() == Some(node)
            && worker.lifecycle_state == Some(RequestLifecycleState::Completed),
        "delegation must use the existing authenticated tool owner after the answer"
    );
    Ok(
        json!({"question":question.doc_id,"answer":answer.doc_id,"worker":worker.doc_id,"signedSourceVerified":true,"editedPromptUsedByWorker":true}),
    )
}
