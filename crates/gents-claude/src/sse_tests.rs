use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Map, Value};

use super::*;

fn decode_all(chunks: &[&[u8]]) -> Result<Vec<SseEvent>, SseError> {
    let mut dec = SseDecoder::new();
    let mut out = Vec::new();
    for c in chunks {
        dec.push(c, &mut out)?;
    }
    dec.finish(&mut out)?;
    Ok(out)
}

fn ev(event: Option<&str>, data: &str) -> SseEvent {
    SseEvent {
        event: event.map(str::to_owned),
        data: data.to_owned(),
    }
}

#[test]
fn every_line_ending_style() {
    for eol in ["\n", "\r\n", "\r"] {
        let text = format!("event: a{eol}data: 1{eol}{eol}event: b{eol}data: 2{eol}{eol}");
        let got = decode_all(&[text.as_bytes()]).unwrap();
        assert_eq!(got, vec![ev(Some("a"), "1"), ev(Some("b"), "2")], "{eol:?}");
    }
}

#[test]
fn cr_at_chunk_end_then_lf_is_one_terminator() {
    let got = decode_all(&[b"data: 1\r", b"\n\r", b"\ndata: 2\n\n"]).unwrap();
    assert_eq!(got, vec![ev(None, "1"), ev(None, "2")]);
}

#[test]
fn comments_missing_colon_and_single_space_strip() {
    let text = ": keepalive\nevent:  ping\ndata\nid: 7\nretry: 5\nunknown: x\ndata:  two\n\n";
    let got = decode_all(&[text.as_bytes()]).unwrap();
    assert_eq!(got, vec![ev(Some(" ping"), "\n two")]);
}

#[test]
fn multi_line_data_joins_with_newline() {
    let got = decode_all(&[b"data: a\ndata: b\ndata: c\n\n"]).unwrap();
    assert_eq!(got, vec![ev(None, "a\nb\nc")]);
}

#[test]
fn blank_line_without_event_is_ignored() {
    assert!(decode_all(&[b"\n\n: c\n\n"]).unwrap().is_empty());
    let got = decode_all(&[b"event: x\n\n"]).unwrap();
    assert_eq!(got, vec![ev(Some("x"), "")]);
}

#[test]
fn utf8_split_across_chunks() {
    let text = "data: h\u{e9}llo \u{1f980}\n\n".as_bytes();
    for cut in 1..text.len() {
        let got = decode_all(&[&text[..cut], &text[cut..]]).unwrap();
        assert_eq!(got, vec![ev(None, "h\u{e9}llo \u{1f980}")], "cut {cut}");
    }
}

#[test]
fn invalid_utf8_becomes_replacement_characters() {
    let got = decode_all(&[b"data: a\xffb\xe2\x82\n\n"]).unwrap();
    assert_eq!(got, vec![ev(None, "a\u{fffd}b\u{fffd}")]);
    let got = decode_all(&[b"data: x\xe2\x82\n", b"\n"]).unwrap();
    assert_eq!(got, vec![ev(None, "x\u{fffd}")]);
}

#[test]
fn leading_bom_is_stripped_from_every_line() {
    let got = decode_all(&["\u{feff}event: a\n\u{feff}data: 1\n\u{feff}\n".as_bytes()]).unwrap();
    assert_eq!(got, vec![ev(Some("a"), "1")]);
}

#[test]
fn empty_event_name_dispatches_only_with_data() {
    assert!(decode_all(&[b"event:\n\n"]).unwrap().is_empty());
    let got = decode_all(&[b"event:\ndata: 1\n\n"]).unwrap();
    assert_eq!(got, vec![ev(Some(""), "1")]);
}

#[test]
fn event_cap_exceeded() {
    let mut dec = SseDecoder::with_max_event_bytes(16);
    let mut out = Vec::new();
    dec.push(b"data: 0123456789\n", &mut out).unwrap();
    let err = dec.push(b"data: 0123456789\n", &mut out).unwrap_err();
    assert!(matches!(err, SseError::EventTooLarge { max: 16 }));
}

#[test]
fn endless_line_is_capped() {
    let mut dec = SseDecoder::with_max_event_bytes(64);
    let mut out = Vec::new();
    let mut result = Ok(());
    for _ in 0..100 {
        result = dec.push(&[b'x'; 10], &mut out);
        if result.is_err() {
            break;
        }
    }
    assert!(matches!(result, Err(SseError::EventTooLarge { .. })));
    assert!(dec.line.len() <= 64);
}

#[test]
fn finish_does_not_dispatch_an_unterminated_event() {
    assert!(decode_all(&[b"event: ping\ndata: {}"]).unwrap().is_empty());
    assert!(decode_all(&[b"event: ping\ndata: {}\n"])
        .unwrap()
        .is_empty());
    assert!(decode_all(&[b"data: x\r"]).unwrap().is_empty());
}

#[test]
fn finish_dispatches_when_the_tail_is_a_blank_line() {
    let got = decode_all(&[b"data: x\r\r"]).unwrap();
    assert_eq!(got, vec![ev(None, "x")]);
}

const SAMPLE: &str = "event: message_start\r\ndata: {\"type\":\"message_start\"}\r\n\r\n\
: c\nevent: ping\ndata: {}\n\nevent: x\rdata: a\rdata: \u{e9}\u{1f980}\r\r\n\
data: tail\n\n";

#[test]
fn random_chunking_matches_whole_decode() {
    let bytes = SAMPLE.as_bytes();
    let whole = decode_all(&[bytes]).unwrap();
    assert_eq!(whole.len(), 4);
    let mut rng = StdRng::seed_from_u64(0x55E);
    for _ in 0..2000 {
        let mut cuts: Vec<usize> = (0..rng.random_range(0..8))
            .map(|_| rng.random_range(0..=bytes.len()))
            .collect();
        cuts.sort_unstable();
        let mut chunks = Vec::new();
        let mut prev = 0;
        for c in cuts {
            chunks.push(&bytes[prev..c]);
            prev = c;
        }
        chunks.push(&bytes[prev..]);
        assert_eq!(decode_all(&chunks).unwrap(), whole, "{chunks:?}");
    }
    let singles: Vec<&[u8]> = bytes.chunks(1).collect();
    assert_eq!(decode_all(&singles).unwrap(), whole);
}

fn sse(name: &str, data: &Value) -> SseEvent {
    ev(Some(name), &data.to_string())
}

fn typed(name: &str, data: Value) -> StreamEvent {
    decode_event(&sse(name, &data)).unwrap().unwrap()
}

#[test]
fn decodes_each_event_name() {
    let start = typed(
        "message_start",
        json!({"type":"message_start","message":{"id":"m"}}),
    );
    assert!(matches!(start, StreamEvent::MessageStart { ref message, .. } if message["id"] == "m"));
    let delta = typed(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3},"zz":1}),
    );
    match delta {
        StreamEvent::MessageDelta {
            stop_reason, extra, ..
        } => {
            assert_eq!(stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(extra["zz"], 1);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        typed("message_stop", json!({"type":"message_stop"})),
        StreamEvent::MessageStop { .. }
    ));
    let cbs = typed(
        "content_block_start",
        json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}),
    );
    assert!(matches!(
        cbs,
        StreamEvent::ContentBlockStart { index: 2, .. }
    ));
    assert!(matches!(
        typed(
            "content_block_stop",
            json!({"type":"content_block_stop","index":2})
        ),
        StreamEvent::ContentBlockStop { index: 2, .. }
    ));
}

#[test]
fn decodes_every_delta_type() {
    let d = |delta: Value| match typed(
        "content_block_delta",
        json!({"type":"content_block_delta","index":0,"delta":delta}),
    ) {
        StreamEvent::ContentBlockDelta { delta, .. } => delta,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        d(json!({"type":"text_delta","text":"a"})),
        Delta::Text { text: "a".into() }
    );
    assert_eq!(
        d(json!({"type":"input_json_delta","partial_json":"{"})),
        Delta::InputJson {
            partial_json: "{".into()
        }
    );
    assert_eq!(
        d(json!({"type":"thinking_delta","thinking":"t"})),
        Delta::Thinking {
            thinking: "t".into()
        }
    );
    assert_eq!(
        d(json!({"type":"signature_delta","signature":"s"})),
        Delta::Signature {
            signature: "s".into()
        }
    );
    assert_eq!(
        d(json!({"type":"citations_delta","citation":{"k":1}})),
        Delta::Citations {
            citation: json!({"k":1})
        }
    );
    assert_eq!(
        d(json!({"type":"compaction_delta","content":"c"})),
        Delta::Compaction {
            content: json!("c"),
            encrypted_content: None
        }
    );
    assert_eq!(
        d(json!({"type":"compaction_delta","content":"c","encrypted_content":null})),
        Delta::Compaction {
            content: json!("c"),
            encrypted_content: Some(Value::Null)
        }
    );
    assert!(matches!(d(json!({"type":"future_delta","x":1})), Delta::Other(m) if m["x"] == 1));
}

#[test]
fn ping_unknown_and_unnamed_are_ignored() {
    assert!(decode_event(&ev(Some("ping"), "{}")).unwrap().is_none());
    assert!(decode_event(&ev(Some("future"), "not json"))
        .unwrap()
        .is_none());
    assert!(decode_event(&ev(None, "{}")).unwrap().is_none());
}

#[test]
fn error_event_is_typed() {
    let e = sse(
        "error",
        &json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
    );
    match decode_event(&e).unwrap().unwrap() {
        StreamEvent::Error {
            error_type,
            message,
            raw,
        } => {
            assert_eq!(error_type.as_deref(), Some("overloaded_error"));
            assert_eq!(message, "Overloaded");
            assert!(raw.contains("overloaded_error"));
        }
        other => panic!("{other:?}"),
    }
    match decode_event(&ev(Some("error"), "upstream down"))
        .unwrap()
        .unwrap()
    {
        StreamEvent::Error {
            error_type,
            message,
            ..
        } => {
            assert_eq!(error_type, None);
            assert_eq!(message, "upstream down");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn malformed_events_error() {
    assert!(matches!(
        decode_event(&ev(Some("message_start"), "{nope")),
        Err(SseError::InvalidJson { .. })
    ));
    assert!(matches!(
        decode_event(&ev(Some("content_block_stop"), "{}")),
        Err(SseError::InvalidEvent { .. })
    ));
    assert!(matches!(
        decode_event(&ev(Some("message_stop"), "[]")),
        Err(SseError::InvalidEvent { .. })
    ));
}

fn start_event() -> StreamEvent {
    typed(
        "message_start",
        json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant",
            "model":"claude-x","content":[],"stop_reason":null,"stop_sequence":null,
            "usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1}}}),
    )
}

fn block_start(index: usize, block: Value) -> StreamEvent {
    typed(
        "content_block_start",
        json!({"type":"content_block_start","index":index,"content_block":block}),
    )
}

fn block_delta(index: usize, delta: Value) -> StreamEvent {
    typed(
        "content_block_delta",
        json!({"type":"content_block_delta","index":index,"delta":delta}),
    )
}

fn block_stop(index: usize) -> StreamEvent {
    typed(
        "content_block_stop",
        json!({"type":"content_block_stop","index":index}),
    )
}

fn stop_event() -> StreamEvent {
    typed("message_stop", json!({"type":"message_stop"}))
}

fn run(events: &[StreamEvent]) -> Result<Message, AccumulateError> {
    let mut acc = MessageAccumulator::new();
    for e in events {
        acc.apply(e)?;
    }
    acc.finish()
}

#[test]
fn realistic_stream_pins_exact_json() {
    let events = [
        start_event(),
        block_start(0, json!({"type":"thinking","thinking":"","signature":""})),
        block_delta(0, json!({"type":"thinking_delta","thinking":"Hm"})),
        block_delta(0, json!({"type":"thinking_delta","thinking":"m"})),
        block_delta(0, json!({"type":"signature_delta","signature":"sig"})),
        block_stop(0),
        block_start(1, json!({"type":"text","text":""})),
        block_delta(1, json!({"type":"text_delta","text":"Hel"})),
        block_delta(1, json!({"type":"text_delta","text":"lo"})),
        block_stop(1),
        block_start(
            2,
            json!({"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}),
        ),
        block_delta(
            2,
            json!({"type":"input_json_delta","partial_json":"{\"command\":"}),
        ),
        block_delta(
            2,
            json!({"type":"input_json_delta","partial_json":"\"ls\"}"}),
        ),
        block_stop(2),
        typed(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
                "usage":{"input_tokens":10,"output_tokens":42,"cache_creation_input_tokens":5,"cache_read_input_tokens":7}}),
        ),
        stop_event(),
    ];
    let msg = run(&events).unwrap();
    assert_eq!(
        serde_json::to_string(msg.as_map()).unwrap(),
        r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","content":[{"type":"thinking","thinking":"Hmm","signature":"sig"},{"type":"text","text":"Hello"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":7,"output_tokens":42}}"#
    );
    assert_eq!(msg.id(), Some("msg_1"));
    assert_eq!(msg.model(), Some("claude-x"));
    assert_eq!(msg.stop_reason(), Some("tool_use"));
    assert_eq!(msg.content().len(), 3);
    let usage = msg.usage().unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens
        ),
        (Some(10), Some(42), Some(7), Some(5))
    );
    assert_eq!(usage.raw.len(), 4);
}

#[test]
fn order_errors() {
    assert!(matches!(
        run(&[block_stop(0)]),
        Err(AccumulateError::BeforeMessageStart {
            event: "content_block_stop"
        })
    ));
    assert!(matches!(
        run(&[block_delta(0, json!({"type":"text_delta","text":"x"}))]),
        Err(AccumulateError::BeforeMessageStart { .. })
    ));
    assert!(matches!(
        run(&[start_event(), start_event()]),
        Err(AccumulateError::DuplicateMessageStart)
    ));
    assert!(matches!(
        run(&[start_event(), stop_event(), start_event()]),
        Err(AccumulateError::DuplicateMessageStart)
    ));
}

#[test]
fn missing_message_stop_is_incomplete() {
    assert!(matches!(
        run(&[start_event()]),
        Err(AccumulateError::IncompleteStream)
    ));
    assert!(matches!(run(&[]), Err(AccumulateError::IncompleteStream)));
}

#[test]
fn fallback_block_sets_model() {
    let msg = run(&[
        start_event(),
        block_start(0, json!({"type":"fallback","to":{"model":"claude-y"}})),
        stop_event(),
    ])
    .unwrap();
    assert_eq!(msg.model(), Some("claude-y"));
    assert_eq!(msg.content().len(), 1);
}

#[test]
fn unknown_delta_and_bad_targets_are_tolerated() {
    let msg = run(&[
        start_event(),
        block_start(0, json!({"type":"text","text":"a"})),
        block_delta(0, json!({"type":"future_delta","x":1})),
        block_delta(0, json!({"type":"thinking_delta","thinking":"ignored"})),
        block_delta(9, json!({"type":"text_delta","text":"out of range"})),
        block_stop(9),
        stop_event(),
    ])
    .unwrap();
    assert_eq!(msg.content()[0], json!({"type":"text","text":"a"}));
}

#[test]
fn citations_and_compaction_deltas() {
    let msg = run(&[
        start_event(),
        block_start(0, json!({"type":"text","text":"","citations":null})),
        block_delta(0, json!({"type":"citations_delta","citation":{"n":1}})),
        block_delta(0, json!({"type":"citations_delta","citation":{"n":2}})),
        block_start(1, json!({"type":"compaction","content":null})),
        block_delta(
            1,
            json!({"type":"compaction_delta","content":"sum","encrypted_content":"enc"}),
        ),
        stop_event(),
    ])
    .unwrap();
    assert_eq!(msg.content()[0]["citations"], json!([{"n":1},{"n":2}]));
    assert_eq!(
        msg.content()[1],
        json!({"type":"compaction","content":"sum","encrypted_content":"enc"})
    );
}

#[test]
fn tool_input_without_deltas_keeps_start_input_and_bad_json_errors() {
    let msg = run(&[
        start_event(),
        block_start(
            0,
            json!({"type":"server_tool_use","id":"s","name":"w","input":{"q":1}}),
        ),
        block_stop(0),
        stop_event(),
    ])
    .unwrap();
    assert_eq!(msg.content()[0]["input"], json!({"q":1}));
    let err = run(&[
        start_event(),
        block_start(
            3 - 3,
            json!({"type":"mcp_tool_use","id":"s","name":"w","input":{}}),
        ),
        block_delta(
            0,
            json!({"type":"input_json_delta","partial_json":"{\"a\":"}),
        ),
        block_stop(0),
    ])
    .unwrap_err();
    assert!(matches!(err, AccumulateError::ToolInput { index: 0, .. }));
}

#[test]
fn message_delta_optional_fields() {
    let msg = run(&[
        start_event(),
        typed(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null,"container":{"id":"c"}},
                "usage":{"output_tokens":9,"server_tool_use":{"n":1},"input_tokens":null},"context_management":{"e":[]}}),
        ),
        stop_event(),
    ])
    .unwrap();
    let m: &Map<String, Value> = msg.as_map();
    assert_eq!(m["container"], json!({"id":"c"}));
    assert_eq!(m["context_management"], json!({"e":[]}));
    assert_eq!(m["usage"]["input_tokens"], 10);
    assert_eq!(m["usage"]["server_tool_use"], json!({"n":1}));
}
