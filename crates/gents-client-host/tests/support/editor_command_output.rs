use super::{fake_llm, ChatAction};
use anyhow::{ensure, Context, Result};
use gents::store::graphql::escape_graphql_string as escape;
use gents::ConfigAccess;
use serde_json::{json, Value};

pub const ACTIVITY_PROMPT: &str = "Run the command-output activity profile.";
pub const ACTIVITY_REPLY: &str = "The twelve command-output fixtures completed.";
const ACTIVITY_COMMANDS: usize = 12;

pub fn reply(request: &Value) -> Option<ChatAction> {
    let messages = request["messages"].as_array()?;
    let has_user = |expected: &str| {
        messages.iter().any(|message| {
            message["role"] == "user"
                && match &message["content"] {
                    Value::String(text) => text.ends_with(expected),
                    Value::Array(parts) => parts.iter().any(|part| {
                        part["text"]
                            .as_str()
                            .is_some_and(|text| text.ends_with(expected))
                    }),
                    _ => false,
                }
        })
    };
    if has_user(ACTIVITY_PROMPT) {
        for index in 0..ACTIVITY_COMMANDS {
            let call = format!("activity-command-{index}");
            if !messages
                .iter()
                .any(|message| message["role"] == "tool" && message["tool_call_id"] == call)
            {
                let payload = "command-output-".repeat(if index == 0 { 2048 } else { 64 });
                let begin = if index == 0 {
                    "printf 'ACTIVITY-0:ready\\n'; while [ ! -f activity-profile-continue ]; do sleep 0.05; done; "
                } else {
                    ""
                };
                let end = if index == 0 {
                    "; while [ ! -f activity-profile-finish ]; do sleep 0.05; done"
                } else {
                    ""
                };
                let script = format!("{begin}i=0; while [ \"$i\" -lt 8 ]; do printf 'ACTIVITY-{index}:%s:%s\\n' \"$i\" '{payload}'; sleep 0.1; i=$((i+1)); done{end}");
                return Some(ChatAction::Sse(fake_llm::tool_call_sse_with_id(
                    &call,
                    "bash_unrestricted",
                    &json!({"command":"/bin/sh", "args":["-c", script], "timeout_secs":60})
                        .to_string(),
                )));
            }
        }
        return Some(ChatAction::Sse(fake_llm::completion_text_sse(
            ACTIVITY_REPLY,
        )));
    }
    None
}

pub async fn verify(
    access: &ConfigAccess,
    node: &str,
    session: &str,
    request: &gents_protocol::row::AgentRequestRow,
) -> Result<Value> {
    gents::verify_request_receipt_signature(request)?;
    ensure!(
        request.node_did.as_deref() == Some(node)
            && request.session_id.as_deref() == Some(session)
            && request.lifecycle_state
                == Some(gents_protocol::request_lifecycle::RequestLifecycleState::Completed),
        "the command profile did not complete in the parent session"
    );
    let document = request
        .doc_id
        .as_deref()
        .context("activity request document")?;
    let response = access.execute(&format!(
            "{{ AgentToolCall(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}, request_doc_id: {{_eq: \"{}\"}}}}) {{ _docID tool_call_id tool_name lifecycle_state }} }}",
            escape(node), escape(session), escape(document)
        )).await?;
    let calls = response["data"]["AgentToolCall"]
        .as_array()
        .context("activity tool records")?;
    ensure!(
        calls.len() == ACTIVITY_COMMANDS,
        "the profile must retain all twelve command records"
    );
    let page = gents_desktop_core::client::load_session_transcript_window_on(
        access,
        session,
        Some(node),
        request.requester_did.as_deref(),
        gents_desktop_core::client::TranscriptWindow::Tip,
        Some(80),
    )
    .await?;
    let segments = page
        .canonical_dependencies
        .output_segments
        .iter()
        .map(
            |row| gents_protocol::output::reconstruction::ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            },
        )
        .collect::<Vec<_>>();
    let mut ids = rapidhash::RapidHashSet::default();
    for call in calls {
        let id = call["tool_call_id"]
            .as_str()
            .context("activity call identity")?;
        ensure!(
            (0..ACTIVITY_COMMANDS).any(|index| id == format!("activity-command-{index}"))
                && ids.insert(id),
            "command profile reused or lost an invocation"
        );
        ensure!(
            call["tool_name"] == "bash_unrestricted" && call["lifecycle_state"] == "completed",
            "command profile failed to retain completed output: {call}"
        );
        let physical = call["_docID"].as_str().context("physical activity call")?;
        let payloads = page
            .store
            .transcript_messages
            .iter()
            .chain(page.result_messages.iter())
            .filter(|row| {
                row.message.node_did == node
                    && row.message.session_id == session
                    && row.message.request_doc_id.as_deref() == Some(document)
                    && row.message.requester_did == request.requester_did
            })
            .flat_map(|row| &row.message.blocks)
            .filter_map(|block| match block {
                gents_protocol::output::MessageBlock::ToolResult {
                    tool_call_doc_id,
                    parts,
                    ..
                } if tool_call_doc_id == physical => Some(parts),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                gents_protocol::output::ToolResultPart::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [payload] = payloads.as_slice() else {
            anyhow::bail!("the activity command has no unique canonical text result: {id}");
        };
        ensure!(
            gents_protocol::output::reconstruction::tool_output_reference_owned(
                &segments,
                &payload.output,
                document,
                physical
            ),
            "activity output crossed its request or physical tool owner"
        );
        let output = gents_protocol::output::reconstruction::reconstruct_stream(
            &segments,
            &[],
            &[],
            &payload.output,
        )?
        .text;
        let index = id
            .strip_prefix("activity-command-")
            .context("activity command prefix")?;
        if index == "0" {
            ensure!(
                output.len() > gents::truncation::TruncationLimits::default().max_bytes,
                "the live command fixture did not exceed the displayed tail limit"
            );
        }
        ensure!(
            output.contains(&format!("ACTIVITY-{index}:0:"))
                && output.contains(&format!("ACTIVITY-{index}:7:")),
            "activity command did not retain its first and final output chunks"
        );
    }
    Ok(
        json!({"requestDocument":document,"completedCommands":calls.len(),"terminalOutputsRetained":true,"largeOutputRetained":true}),
    )
}
