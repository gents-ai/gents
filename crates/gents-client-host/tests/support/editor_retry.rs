use super::{fake_llm, Attachment, ChatAction};
use crate::support::TestResult;
use gents::ConfigAccess;
use gents_protocol::{request_lifecycle::RequestLifecycleState, row::AgentRequestRow};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const PROMPT: &str = "Retry this failed editor turn.";
pub const REPLY: &str = "The original editor input completed after retry.";
pub const CAPTURED: &str = "original retry captured context\n";

fn is_retry_input(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .any(|message| match &message["content"] {
            Value::String(text) => text == PROMPT,
            Value::Array(parts) => parts.iter().any(|part| part["text"] == PROMPT),
            _ => false,
        })
}

pub fn reply(request: &Value, attempts: &AtomicUsize) -> Option<ChatAction> {
    if !is_retry_input(request) {
        return None;
    }
    Some(if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
        ChatAction::HttpError(
            axum::http::StatusCode::BAD_REQUEST,
            json!({"error":{"message":"Retry fixture provider refusal", "type":"invalid_request_error"}}).to_string(),
        )
    } else {
        ChatAction::Sse(fake_llm::completion_text_sse(REPLY))
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn verify(
    client: &mut Attachment,
    id: &mut u32,
    access: &ConfigAccess,
    provider: &fake_llm::FakeLlm,
    root: &Path,
    node: &str,
    session: &str,
    author: &str,
) -> TestResult<Value> {
    let ui: Value = serde_json::from_slice(&std::fs::read(root.join("editor-retry-ui.json"))?)?;
    let before = requests(access, node, session).await?;
    assert!(
        before.len() == 3,
        "retry must retain the initial turn and admit only one successor"
    );
    let retried = before
        .iter()
        .filter(|row| row.content.as_deref() == Some(PROMPT))
        .collect::<Vec<_>>();
    assert!(
        retried.len() == 2,
        "the failed input must have exactly one retry"
    );
    let parent = retried
        .iter()
        .find(|row| row.lifecycle_state == Some(RequestLifecycleState::Failed))
        .expect("failed retry parent");
    let child = retried
        .iter()
        .find(|row| row.lifecycle_state == Some(RequestLifecycleState::Completed))
        .expect("completed retry successor");
    for row in &before {
        gents::verify_request_receipt_signature(row)?;
        assert!(
            row.admission_signer_did.as_deref() == Some(author)
                && row.requester_did.as_deref() == Some(author),
            "the retry escaped the enrolled client authority"
        );
    }
    assert!(
        child.retry_parent_request.as_deref() == Some(parent.request_id.as_str())
            && child.retry_parent_request_doc_id == parent.doc_id
            && child.retry_root_request.as_deref() == Some(parent.request_id.as_str())
            && child.input == parent.input,
        "the retry must preserve exact signed ancestry and typed input"
    );
    let input = parent.input.as_ref().expect("original typed retry input");
    assert!(
        input.captured_context.len() == 1,
        "original retry capture count"
    );
    let capture = serde_json::to_value(&input.captured_context[0])?;
    assert!(
        capture["content"] == CAPTURED
            && capture["uri"] == ui["contextUri"]
            && capture["kind"] == "selection"
            && capture["unsaved"] == true,
        "retry lost the captured unsaved input"
    );
    let repeated = client
        .try_call(
            *id,
            "desktop_request_retry",
            json!({
                "nodeDid":node, "requestId":parent.request_id,
            }),
        )
        .await?;
    *id += 1;
    assert!(
        repeated["requestId"] == child.request_id
            && repeated["sessionId"] == session
            && repeated["nodeDid"] == node,
        "a repeated retry created a different successor"
    );
    for (scope, request) in [
        (author, parent.request_id.as_str()),
        (node, child.request_id.as_str()),
    ] {
        let refusal = client
            .try_call(
                *id,
                "desktop_request_retry",
                json!({
                    "nodeDid":scope, "requestId":request,
                }),
            )
            .await;
        *id += 1;
        assert!(
            refusal.is_err(),
            "retry accepted a wrong scope or completed parent"
        );
    }
    let after = requests(access, node, session).await?;
    assert!(
        after.len() == before.len(),
        "refused or repeated retry admitted extra work"
    );
    let captured = provider.captured_chat_requests();
    let attempts = captured
        .iter()
        .filter(|request| is_retry_input(request))
        .collect::<Vec<_>>();
    let provider_attempts = attempts.len();
    assert!(
        provider_attempts == 2,
        "retry must make exactly one additional provider attempt"
    );
    for request in attempts {
        let inputs = request["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| message["role"] == "user")
            .filter_map(|message| message["content"].as_array())
            .filter(|parts| parts.iter().any(|part| part["text"] == PROMPT))
            .collect::<Vec<_>>();
        assert!(
            inputs.len() == 1,
            "provider input duplicated the authored retry input"
        );
        let parts = inputs[0];
        assert!(
            parts.len() == 2 && parts[0]["text"] == PROMPT,
            "provider retry input shape"
        );
        let context = parts[1]["text"]
            .as_str()
            .and_then(|text| {
                text.strip_prefix(
                    "Editor context (captured text; URI identifies its editor location):\n",
                )
            })
            .expect("provider retry context");
        assert!(
            serde_json::from_str::<Value>(context)? == capture,
            "provider received a different retry snapshot"
        );
    }
    Ok(json!({
        "nodeDid":node, "sessionId":session, "authorDid":author,
        "parentRequestId":parent.request_id, "retryRequestId":child.request_id,
        "verifiedSignatures":before.len(), "providerAttempts":provider_attempts,
        "typedInputPreserved":true, "idempotentSuccessorVerified":true,
        "wrongScopeRefused":true, "completedParentRefused":true,
        "ui":ui,
    }))
}

async fn requests(
    access: &ConfigAccess,
    node: &str,
    session: &str,
) -> TestResult<Vec<AgentRequestRow>> {
    let result = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}, purpose: {{_eq: "normal"}}}}) {{ {} }} }}"#,
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(session),
        gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    Ok(serde_json::from_value(
        result["data"]["AgentRequest"].clone(),
    )?)
}
