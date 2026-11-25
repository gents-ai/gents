use std::convert::Infallible;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;
use serde_json::{json, Value};

use super::{
    json_response, InFlight, Release, StreamChunk, StreamScript, StreamingState, DEFAULT_REPLY,
};

#[derive(Clone, Copy)]
pub(super) enum Wire {
    ChatCompletions,
    Messages,
    Responses,
}

/// How one completion is answered: the wire its route speaks, whether it
/// asked for a stream, and its position in the request log, which the
/// Responses wire's item ids carry.
#[derive(Clone, Copy)]
pub(super) struct Reply {
    pub(super) wire: Wire,
    pub(super) streaming: bool,
    pub(super) ordinal: usize,
}

pub(super) fn closed_stream() -> Response {
    Sse::new(futures::stream::empty::<Result<Event, Infallible>>()).into_response()
}

/// The reasoning model's turn for `body`, when it offers a tool.
pub(super) fn reasoning_tool_turn(body: &Value, ordinal: usize) -> Option<StreamScript> {
    let tool = body["tools"][0]["name"]
        .as_str()
        .or_else(|| body["tools"][0]["function"]["name"].as_str())?;
    let answered = body["input"]
        .as_array()
        .and_then(|input| input.last())
        .is_some_and(|item| item["type"] == "function_call_output")
        || body["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .is_some_and(|message| message["role"] == "tool");
    let last = if answered {
        StreamChunk::text(DEFAULT_REPLY)
    } else {
        StreamChunk::tool_call(format!("call_{ordinal}"), tool, "{}")
    };
    Some(StreamScript::streams(
        "__reasoning_tool_turn__",
        vec![StreamChunk::reasoning(""), last],
    ))
}

pub(super) fn stream(
    script: StreamScript,
    state: Arc<StreamingState>,
    reply: Reply,
    in_flight: InFlight,
) -> Response {
    if !reply.streaming {
        return json_response(StatusCode::OK, complete_reply(&state, reply, &script));
    }

    #[derive(Clone, Copy)]
    enum Phase {
        Start,
        Chunk { index: usize, released: bool },
        Finish,
        Finished,
    }

    struct Cursor {
        phase: Phase,
        script: StreamScript,
        state: Arc<StreamingState>,
        sequence: usize,
        in_flight: InFlight,
    }

    let init = Cursor {
        phase: Phase::Start,
        script,
        state,
        sequence: 0,
        in_flight,
    };
    let stream = futures::stream::unfold(init, move |mut cursor| async move {
        loop {
            let events = match cursor.phase {
                Phase::Start => {
                    cursor.phase = Phase::Chunk {
                        index: 0,
                        released: false,
                    };
                    start_events(&cursor.state, reply)
                }
                Phase::Chunk { index, released } => {
                    if !released && cursor.script.pause_at == Some(index) {
                        match cursor.state.released(&cursor.script.marker).await {
                            Some(Release::Complete) => {
                                cursor.phase = Phase::Chunk {
                                    index,
                                    released: true,
                                };
                                continue;
                            }
                            Some(Release::Fail { message, .. }) => {
                                cursor.phase = Phase::Finished;
                                cursor.in_flight.end();
                                error_events(reply.wire, &message)
                            }
                            Some(Release::Close) | None => return None,
                        }
                    } else if cursor.state.stopped() {
                        return None;
                    } else if let Some(chunk) = cursor.script.chunks.get(index) {
                        cursor.state.record_chunk(&cursor.script.marker);
                        cursor.phase = Phase::Chunk {
                            index: index + 1,
                            released,
                        };
                        chunk_events(reply, &cursor.script, index, chunk)
                    } else {
                        cursor.phase = Phase::Finish;
                        continue;
                    }
                }
                Phase::Finish => {
                    cursor.phase = Phase::Finished;
                    cursor.in_flight.end();
                    finish_events(&cursor.state, reply, &cursor.script)
                }
                Phase::Finished => return None,
            };
            if events.is_empty() {
                continue;
            }
            let events = events
                .into_iter()
                .map(|(name, mut value)| {
                    if matches!(reply.wire, Wire::Responses) {
                        value["sequence_number"] = json!(cursor.sequence);
                        cursor.sequence += 1;
                    }
                    let data = match value {
                        Value::String(raw) => raw,
                        value => value.to_string(),
                    };
                    match name {
                        Some(name) => Event::default().event(name).data(data),
                        None => Event::default().data(data),
                    }
                })
                .collect::<Vec<_>>();
            return Some((events, cursor));
        }
    })
    .flat_map(|events| futures::stream::iter(events.into_iter().map(Ok::<Event, Infallible>)));

    Sse::new(stream).into_response()
}

/// An SSE event: its name (the Messages and Responses wires name theirs) and
/// its data, a JSON string being sent raw.
type WireEvent = (Option<&'static str>, Value);

fn data(value: Value) -> WireEvent {
    (None, value)
}

fn named(name: &'static str, value: Value) -> WireEvent {
    (Some(name), value)
}

fn typed(value: Value) -> WireEvent {
    let name = match value["type"].as_str() {
        Some("response.output_item.added") => "response.output_item.added",
        Some("response.output_item.done") => "response.output_item.done",
        Some("response.output_text.delta") => "response.output_text.delta",
        Some("response.completed") => "response.completed",
        _ => "error",
    };
    (Some(name), value)
}

fn without_usage(state: &StreamingState, mut value: Value) -> Value {
    if !state.usage {
        if let Some(object) = value.as_object_mut() {
            object.remove("usage");
        }
    }
    value
}

/// The Responses wire's output items for `script`, in order: every text
/// chunk joins one message, placed where its first chunk is.
fn responses_items(script: &StreamScript, ordinal: usize) -> Vec<(usize, Value)> {
    let mut items = Vec::new();
    let mut message = None;
    for (index, chunk) in script.chunks.iter().enumerate() {
        match chunk {
            StreamChunk::Text(_) if message.is_none() => {
                message = Some(items.len());
                items.push((index, responses_message(ordinal, &script.text())));
            }
            StreamChunk::Text(_) => {}
            StreamChunk::Reasoning(reasoning) => items.push((
                index,
                json!({
                    "type": "reasoning", "id": format!("rs_{ordinal}"),
                    "summary": if reasoning.is_empty() {
                        json!([])
                    } else {
                        json!([{ "type": "summary_text", "text": reasoning }])
                    },
                    "encrypted_content": format!("sealed-{ordinal}"),
                }),
            )),
            StreamChunk::ToolCall {
                id,
                name,
                arguments,
            } => items.push((
                index,
                json!({
                    "type": "function_call", "id": format!("fc_{ordinal}"),
                    "call_id": id, "name": name, "arguments": arguments,
                    "status": "completed",
                }),
            )),
        }
    }
    items
}

fn responses_message(ordinal: usize, text: &str) -> Value {
    json!({
        "type": "message", "id": format!("msg_{ordinal}"), "role": "assistant",
        "status": "completed",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
    })
}

fn responses_usage() -> Value {
    json!({
        "input_tokens": 10, "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": 2, "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": 12,
    })
}

fn responses_object(state: &StreamingState, ordinal: usize, output: Vec<Value>) -> Value {
    without_usage(
        state,
        json!({
            "id": format!("resp_{ordinal}"), "object": "response", "created_at": 1,
            "status": "completed", "error": null, "incomplete_details": null,
            "instructions": null, "max_output_tokens": null, "model": state.model_name(),
            "output": output,
            "usage": responses_usage(),
        }),
    )
}

/// The non-streamed reply to a completion, carrying the script's text.
fn complete_reply(state: &StreamingState, reply: Reply, script: &StreamScript) -> String {
    let text = script.text();
    let value = match reply.wire {
        Wire::ChatCompletions => json!({
            "id": "chatcmpl-scripted",
            "object": "chat.completion",
            "created": 1_710_000_000_u64,
            "model": state.model_name(),
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": text,
                    "refusal": null,
                    "reasoning": null
                }
            }],
            "usage": { "prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5 }
        }),
        Wire::Messages => json!({
            "id": "msg_scripted",
            "type": "message",
            "role": "assistant",
            "model": state.model_name(),
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": { "input_tokens": 4, "output_tokens": 1 }
        }),
        Wire::Responses => {
            return responses_object(
                state,
                reply.ordinal,
                vec![responses_message(reply.ordinal, &text)],
            )
            .to_string();
        }
    };
    without_usage(state, value).to_string()
}

fn start_events(state: &StreamingState, reply: Reply) -> Vec<WireEvent> {
    match reply.wire {
        Wire::ChatCompletions | Wire::Responses => Vec::new(),
        Wire::Messages => {
            let message = without_usage(
                state,
                json!({
                    "id": "msg_scripted",
                    "type": "message",
                    "role": "assistant",
                    "model": state.model_name(),
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 8, "output_tokens": 0 }
                }),
            );
            vec![named(
                "message_start",
                json!({ "type": "message_start", "message": message }),
            )]
        }
    }
}

fn chunk_events(
    reply: Reply,
    script: &StreamScript,
    index: usize,
    chunk: &StreamChunk,
) -> Vec<WireEvent> {
    match reply.wire {
        Wire::ChatCompletions => vec![data(chat_chunk(index, chunk))],
        Wire::Messages => messages_chunk(index, chunk),
        Wire::Responses => {
            let items = responses_items(script, reply.ordinal);
            let Some(output_index) = items.iter().position(|(first, _)| *first == index) else {
                let message = items
                    .iter()
                    .position(|(_, item)| item["type"] == "message")
                    .unwrap_or_default();
                return responses_delta(reply.ordinal, message, chunk);
            };
            let item = &items[output_index].1;
            match chunk {
                StreamChunk::Text(_) => responses_delta(reply.ordinal, output_index, chunk),
                StreamChunk::Reasoning(_) => vec![typed(json!({
                    "type": "response.output_item.done", "output_index": output_index,
                    "item": item,
                }))],
                StreamChunk::ToolCall { .. } => vec![
                    typed(json!({
                        "type": "response.output_item.added", "output_index": output_index,
                        "item": item,
                    })),
                    typed(json!({
                        "type": "response.output_item.done", "output_index": output_index,
                        "item": item,
                    })),
                ],
            }
        }
    }
}

fn responses_delta(ordinal: usize, output_index: usize, chunk: &StreamChunk) -> Vec<WireEvent> {
    let StreamChunk::Text(text) = chunk else {
        return Vec::new();
    };
    vec![typed(json!({
        "type": "response.output_text.delta", "item_id": format!("msg_{ordinal}"),
        "output_index": output_index, "content_index": 0, "delta": text,
    }))]
}

fn messages_chunk(index: usize, chunk: &StreamChunk) -> Vec<WireEvent> {
    let (block, delta) = match chunk {
        StreamChunk::Text(text) => (
            json!({ "type": "text", "text": "" }),
            vec![json!({ "type": "text_delta", "text": text })],
        ),
        StreamChunk::Reasoning(reasoning) => (
            json!({ "type": "thinking", "thinking": "" }),
            vec![
                json!({ "type": "thinking_delta", "thinking": reasoning }),
                json!({ "type": "signature_delta", "signature": "scripted" }),
            ],
        ),
        StreamChunk::ToolCall {
            id,
            name,
            arguments,
        } => (
            json!({ "type": "tool_use", "id": id, "name": name, "input": {} }),
            vec![json!({ "type": "input_json_delta", "partial_json": arguments })],
        ),
    };
    let mut events = vec![named(
        "content_block_start",
        json!({ "type": "content_block_start", "index": index, "content_block": block }),
    )];
    events.extend(delta.into_iter().map(|delta| {
        named(
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": index, "delta": delta }),
        )
    }));
    events.push(named(
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": index }),
    ));
    events
}

fn chat_chunk(index: usize, chunk: &StreamChunk) -> Value {
    let delta = match chunk {
        StreamChunk::Text(content) => json!({ "content": content, "tool_calls": [] }),
        StreamChunk::Reasoning(reasoning) => {
            json!({ "content": null, "reasoning_content": reasoning, "tool_calls": [] })
        }
        StreamChunk::ToolCall {
            id,
            name,
            arguments,
        } => json!({
            "content": null,
            "tool_calls": [{
                "index": index,
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": arguments }
            }]
        }),
    };
    json!({
        "choices": [{ "delta": delta, "finish_reason": null }],
        "usage": null
    })
}

fn finish_events(state: &StreamingState, reply: Reply, script: &StreamScript) -> Vec<WireEvent> {
    match reply.wire {
        Wire::ChatCompletions => {
            let mut events = Vec::new();
            if state.usage {
                events.push(data(json!({
                    "choices": [],
                    "usage": { "prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11 }
                })));
            }
            events.push((None, Value::String("[DONE]".to_owned())));
            events
        }
        Wire::Messages => {
            let stop_reason = if script.ends_with_tool_call() {
                "tool_use"
            } else {
                "end_turn"
            };
            let delta = without_usage(
                state,
                json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                    "usage": { "output_tokens": 3 }
                }),
            );
            vec![
                named("message_delta", delta),
                named("message_stop", json!({ "type": "message_stop" })),
            ]
        }
        Wire::Responses => {
            let items = responses_items(script, reply.ordinal);
            let mut events = items
                .iter()
                .enumerate()
                .filter(|(_, (_, item))| item["type"] == "message")
                .map(|(output_index, (_, item))| {
                    typed(json!({
                        "type": "response.output_item.done", "output_index": output_index,
                        "item": item,
                    }))
                })
                .collect::<Vec<_>>();
            let output = items.into_iter().map(|(_, item)| item).collect();
            events.push(typed(json!({
                "type": "response.completed",
                "response": responses_object(state, reply.ordinal, output),
            })));
            events
        }
    }
}

fn error_events(wire: Wire, message: &str) -> Vec<WireEvent> {
    match wire {
        Wire::ChatCompletions => vec![data(
            json!({ "error": { "message": message, "type": "server_error" } }),
        )],
        Wire::Messages => vec![named(
            "error",
            json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
        )],
        Wire::Responses => vec![typed(json!({
            "type": "error", "code": "server_error", "message": message,
        }))],
    }
}
