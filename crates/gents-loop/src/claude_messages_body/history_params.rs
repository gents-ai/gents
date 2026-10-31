//! The reserved request parameters that carry what the Rig chat history
//! cannot: the previous response's request id and the tool calls' streamed
//! argument text. Both exist only on a request that carries the Claude request
//! context, so no other provider ever sees a reserved key, and neither reaches
//! the wire (the body is typed).

use gents_protocol::message::{AssistantContent, Message};
use serde_json::{Map, Value};

use super::CLAUDE_REQUEST_PARAM;

/// Request parameter (never sent): the `request_id` of the last assistant
/// message in the history that has one (Claude Code's `cc_prev_req`).
pub const PREVIOUS_REQUEST_ID_PARAM: &str = "_gents_previous_request_id";

/// Request parameter (never sent): tool call id to the provider's streamed
/// argument text, for history tool calls that kept it.
pub const RAW_TOOL_ARGUMENTS_BY_CALL_PARAM: &str = "_gents_raw_tool_arguments";

/// Sets both reserved parameters from the history about to be sent, clearing
/// stale values; does nothing unless `params` carries the Claude request context.
pub(crate) fn stamp_history_params(params: &mut Option<Value>, history: &[Message]) {
    let Some(Value::Object(map)) = params else {
        return;
    };
    if !map.contains_key(CLAUDE_REQUEST_PARAM) {
        return;
    }
    map.remove(PREVIOUS_REQUEST_ID_PARAM);
    map.remove(RAW_TOOL_ARGUMENTS_BY_CALL_PARAM);
    let previous = history.iter().rev().find_map(|message| match message {
        Message::Assistant {
            request_id: Some(request_id),
            ..
        } => Some(request_id.clone()),
        _ => None,
    });
    if let Some(request_id) = previous {
        map.insert(
            PREVIOUS_REQUEST_ID_PARAM.to_owned(),
            Value::String(request_id),
        );
    }
    let raw: Map<String, Value> = history
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            AssistantContent::ToolCall(call) => call
                .raw_arguments
                .as_ref()
                .map(|text| (call.id.clone(), Value::String(text.clone()))),
            _ => None,
        })
        .collect();
    if !raw.is_empty() {
        map.insert(
            RAW_TOOL_ARGUMENTS_BY_CALL_PARAM.to_owned(),
            Value::Object(raw),
        );
    }
}

/// The previous response's request id a request carries.
pub(super) fn previous_request_id(params: Option<&Value>) -> Option<&str> {
    params?.get(PREVIOUS_REQUEST_ID_PARAM)?.as_str()
}

/// Re-attaches the streamed argument text a request carries to the history's
/// tool calls, which the Rig round trip dropped; the text is kept only where
/// the shared owner accepts it for the call's arguments.
pub(super) fn attach_raw_arguments(history: &mut [Message], params: Option<&Value>) {
    let Some(raw) = params
        .and_then(|params| params.get(RAW_TOOL_ARGUMENTS_BY_CALL_PARAM))
        .and_then(Value::as_object)
    else {
        return;
    };
    for message in history {
        let Message::Assistant { content, .. } = message else {
            continue;
        };
        for item in content {
            if let AssistantContent::ToolCall(call) = item {
                call.raw_arguments = raw.get(&call.id).and_then(Value::as_str).and_then(|text| {
                    gents_protocol::message::ToolCall::raw_arguments_for(
                        &call.function.arguments,
                        text,
                    )
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gents_protocol::message::{ToolCall, ToolFunction};
    use serde_json::json;

    use super::*;

    fn assistant(request_id: Option<&str>, calls: &[(&str, Option<&str>)]) -> Message {
        Message::Assistant {
            id: Some("msg_x".into()),
            request_id: request_id.map(str::to_owned),
            content: calls
                .iter()
                .map(|(id, raw)| {
                    let mut call =
                        ToolCall::new((*id).into(), ToolFunction::new("t".into(), json!({})));
                    call.raw_arguments = raw.map(str::to_owned);
                    AssistantContent::ToolCall(call)
                })
                .collect(),
        }
    }

    fn claude_params() -> Option<Value> {
        Some(json!({ CLAUDE_REQUEST_PARAM: {"origin": 1}, "keep": true }))
    }

    #[test]
    fn the_last_assistant_request_id_and_every_raw_text_are_stamped() {
        let history = vec![
            Message::user("a"),
            assistant(Some("req_1"), &[("c1", Some("{ }"))]),
            Message::user("b"),
            assistant(Some("req_2"), &[("c2", None), ("c3", Some("{ \"a\":1 }"))]),
            // A later assistant row without an id leaves the last known one.
            assistant(None, &[]),
            Message::user("c"),
        ];
        let mut params = claude_params();
        stamp_history_params(&mut params, &history);
        let params = params.unwrap();
        assert_eq!(params[PREVIOUS_REQUEST_ID_PARAM], "req_2");
        assert_eq!(
            params[RAW_TOOL_ARGUMENTS_BY_CALL_PARAM],
            json!({"c1": "{ }", "c3": "{ \"a\":1 }"})
        );
        assert_eq!(params["keep"], true);
    }

    #[test]
    fn a_history_without_ids_clears_stale_values() {
        let mut params = claude_params();
        stamp_history_params(
            &mut params,
            &[assistant(Some("req_1"), &[("c", Some("{ }"))])],
        );
        // A compacted history keeps no assistant row that has either.
        stamp_history_params(
            &mut params,
            &[Message::user("summary"), assistant(None, &[("c", None)])],
        );
        let params = params.unwrap();
        assert!(params.get(PREVIOUS_REQUEST_ID_PARAM).is_none());
        assert!(params.get(RAW_TOOL_ARGUMENTS_BY_CALL_PARAM).is_none());
        assert_eq!(params["keep"], true);
    }

    #[test]
    fn requests_without_the_claude_context_get_nothing() {
        let history = vec![assistant(Some("req_1"), &[("c", Some("{ }"))])];
        for mut params in [None, Some(json!({"keep": true})), Some(json!("scalar"))] {
            let before = params.clone();
            stamp_history_params(&mut params, &history);
            assert_eq!(params, before);
        }
    }
}
