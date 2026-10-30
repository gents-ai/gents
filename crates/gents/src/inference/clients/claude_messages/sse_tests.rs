use rapidhash::RapidHashSet;
use rig::streaming::RawStreamingChoice;
use serde_json::Value;

use super::MessagesSseState;

#[test]
fn the_parser_keeps_the_streamed_tool_text_and_the_response_request_id() {
    let streamed = "{\"z\": 1, \"a\": [true, null]}";
    let partial = serde_json::to_string(streamed).expect("escape partial_json");
    let sse = format!(
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo\",\"input\":{{}}}}}}\n\n\
         event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{partial}}}}}\n\n\
         event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    );
    let mut state = MessagesSseState::new(RapidHashSet::from_iter(["echo".to_string()]))
        .with_request_id(Some("req_011".into()));
    let mut events = Vec::new();
    for line in sse.lines() {
        events.extend(state.push_line(line).expect("push"));
    }
    let [RawStreamingChoice::ToolCall(call), RawStreamingChoice::FinalResponse(response)] =
        &events[..]
    else {
        panic!("expected a tool call then the final response: {events:?}");
    };
    assert_eq!(
        call.additional_params
            .as_ref()
            .and_then(|params| params.get(gents_loop::rig_compat::RAW_TOOL_ARGUMENTS_PARAM))
            .and_then(Value::as_str),
        Some(streamed)
    );
    assert_eq!(
        gents_loop::rig_compat::observed_request_id(response).as_deref(),
        Some("req_011")
    );
}
