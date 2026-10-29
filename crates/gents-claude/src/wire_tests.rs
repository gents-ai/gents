use serde_json::{json, Map, Value};

use super::*;

fn marked() -> Option<CacheControl> {
    Some(CacheControl { ttl: None })
}

fn text(t: &str) -> ContentBlock {
    ContentBlock::Text {
        text: t.to_owned(),
        citations: None,
        cache_control: None,
    }
}

fn request(messages: Vec<Message>) -> MessagesRequest {
    MessagesRequest {
        model: "claude-opus-5-5".to_owned(),
        messages,
        system: vec![
            SystemBlock {
                text: "x-anthropic-billing-header: cc_version=2.1.311;".to_owned(),
                cache_control: None,
            },
            SystemBlock {
                text: "You are Claude Code.".to_owned(),
                cache_control: marked(),
            },
            SystemBlock {
                text: "Be brief.".to_owned(),
                cache_control: marked(),
            },
        ],
        tools: Some(vec![]),
        metadata: Metadata {
            user_id: "u".to_owned(),
        },
        max_tokens: 128000,
        thinking: Some(Thinking::Adaptive {
            display: Some(ThinkingDisplay::Updates),
        }),
        temperature: None,
        context_management: Some(ContextManagement {
            edits: vec![ContextEdit::ClearThinking],
        }),
        output_config: Some(OutputConfig {
            effort: Some("medium".to_owned()),
            format: None,
        }),
        diagnostics: Some(Diagnostics {
            previous_message_id: None,
        }),
        stream: true,
    }
}

fn s(request: &MessagesRequest) -> String {
    String::from_utf8(to_bytes(request).unwrap()).unwrap()
}

const TAIL: &str = r#""tools":[],"metadata":{"user_id":"u"},"max_tokens":128000,"thinking":{"type":"adaptive","display":"updates"},"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]},"output_config":{"effort":"medium"},"diagnostics":{"previous_message_id":null},"stream":true}"#;
const SYSTEM: &str = r#""system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.311;"},{"type":"text","text":"You are Claude Code.","cache_control":{"type":"ephemeral"}},{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],"#;

#[test]
fn default_main_thread_body_bytes() {
    let msg = Message {
        role: Role::User,
        content: MessageContent::Blocks(vec![ContentBlock::Text {
            text: "hi".to_owned(),
            citations: None,
            cache_control: marked(),
        }]),
    };
    let expected = format!(
        r#"{{"model":"claude-opus-5-5","messages":[{{"role":"user","content":[{{"type":"text","text":"hi","cache_control":{{"type":"ephemeral"}}}}]}}],{SYSTEM}{TAIL}"#
    );
    assert_eq!(s(&request(vec![msg])), expected);
}

#[test]
fn tool_round_bytes() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: MessageContent::Blocks(vec![
                ContentBlock::Thinking {
                    thinking: "hm".to_owned(),
                    signature: "sig".to_owned(),
                },
                text("ok"),
                ContentBlock::ToolUse {
                    id: "toolu_1".to_owned(),
                    name: "Bash".to_owned(),
                    input: json!({"z": 1, "a": {"y": [1, 2], "b": null}}),
                    cache_control: None,
                },
            ]),
        },
        Message {
            role: Role::User,
            content: MessageContent::Blocks(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".to_owned(),
                    content: ToolResultContent::Text("boom".to_owned()),
                    is_error: Some(true),
                    cache_control: None,
                },
                text("next"),
            ]),
        },
    ];
    let expected = format!(
        r#"{{"model":"claude-opus-5-5","messages":[{{"role":"assistant","content":[{{"type":"thinking","thinking":"hm","signature":"sig"}},{{"type":"text","text":"ok"}},{{"type":"tool_use","id":"toolu_1","name":"Bash","input":{{"z":1,"a":{{"y":[1,2],"b":null}}}}}}]}},{{"role":"user","content":[{{"tool_use_id":"toolu_1","type":"tool_result","content":"boom","is_error":true}},{{"type":"text","text":"next"}}]}}],{SYSTEM}{TAIL}"#
    );
    assert_eq!(s(&request(messages)), expected);
}

fn user_text(t: &str) -> MessagesRequest {
    request(vec![Message {
        role: Role::User,
        content: MessageContent::Text(t.to_owned()),
    }])
}

#[test]
fn string_escaping_matches_json_stringify() {
    let out = s(&user_text(
        "q\" b\\ \u{8}\u{c}\n\r\t\u{1}\u{1f} \u{2028}\u{2029}\u{7f} 😀 </script> é",
    ));
    let want = "\"content\":\"q\\\" b\\\\ \\b\\f\\n\\r\\t\\u0001\\u001f \u{2028}\u{2029}\u{7f} 😀 </script> é\"";
    assert!(out.contains(want), "{out}");
}

#[test]
fn tool_result_key_order_and_block_body() {
    let block = ContentBlock::ToolResult {
        tool_use_id: "t".to_owned(),
        content: ToolResultContent::Blocks(vec![text("a")]),
        is_error: None,
        cache_control: Some(CacheControl {
            ttl: Some(CacheTtl::OneHour),
        }),
    };
    assert_eq!(
        serde_json::to_string(&block).unwrap(),
        r#"{"tool_use_id":"t","type":"tool_result","content":[{"type":"text","text":"a"}],"cache_control":{"type":"ephemeral","ttl":"1h"}}"#
    );
}

#[test]
fn raw_block_appends_cache_control_last() {
    let mut map = Map::new();
    map.insert("type".to_owned(), json!("server_tool_use"));
    map.insert("id".to_owned(), json!("srvtoolu_1"));
    map.insert(
        "cache_control".to_owned(),
        json!({"type": "ephemeral", "ttl": "1h"}),
    );
    map.insert("name".to_owned(), json!("web_search"));
    let mut block = ContentBlock::Raw(map);
    assert_eq!(block.kind(), "server_tool_use");
    assert!(block.set_cache_control(CacheControl { ttl: None }));
    assert_eq!(
        serde_json::to_string(&block).unwrap(),
        r#"{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","cache_control":{"type":"ephemeral"}}"#
    );
    assert_eq!(block.cache_control(), Some(&CacheControl { ttl: None }));
}

#[test]
fn thinking_blocks_refuse_a_marker() {
    let mut t = ContentBlock::Thinking {
        thinking: String::new(),
        signature: String::new(),
    };
    assert!(!t.set_cache_control(CacheControl { ttl: None }));
    let mut r = ContentBlock::RedactedThinking {
        data: String::new(),
    };
    assert!(!r.set_cache_control(CacheControl { ttl: None }));
    assert_eq!(r.kind(), "redacted_thinking");
}

#[test]
fn thinking_modes_bytes() {
    let enabled = Thinking::Enabled {
        budget_tokens: 31999,
        display: Some(ThinkingDisplay::Updates),
    };
    assert_eq!(
        serde_json::to_string(&enabled).unwrap(),
        r#"{"budget_tokens":31999,"type":"enabled","display":"updates"}"#
    );
    assert_eq!(
        serde_json::to_string(&Thinking::Enabled {
            budget_tokens: 1,
            display: None
        })
        .unwrap(),
        r#"{"budget_tokens":1,"type":"enabled"}"#
    );
    assert_eq!(
        serde_json::to_string(&Thinking::Disabled).unwrap(),
        r#"{"type":"disabled"}"#
    );
    assert_eq!(
        serde_json::to_string(&Thinking::Adaptive { display: None }).unwrap(),
        r#"{"type":"adaptive"}"#
    );
}

#[test]
fn tool_schema_keeps_insertion_order() {
    let schema: Value = serde_json::from_str(
        r#"{"type":"object","properties":{"zeta":{"type":"string"},"alpha":{"type":"integer"}},"required":["zeta"],"additionalProperties":false}"#,
    )
    .unwrap();
    let tool = Tool {
        name: "Read".to_owned(),
        description: "d".to_owned(),
        input_schema: schema,
    };
    assert_eq!(
        serde_json::to_string(&tool).unwrap(),
        r#"{"name":"Read","description":"d","input_schema":{"type":"object","properties":{"zeta":{"type":"string"},"alpha":{"type":"integer"}},"required":["zeta"],"additionalProperties":false}}"#
    );
}

#[test]
fn media_blocks_and_optional_keys() {
    let image = ContentBlock::Image {
        media_type: "image/png".to_owned(),
        data: "AAA".to_owned(),
        cache_control: marked(),
    };
    assert_eq!(
        serde_json::to_string(&image).unwrap(),
        r#"{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAA"},"cache_control":{"type":"ephemeral"}}"#
    );
    let doc = ContentBlock::Document {
        media_type: "application/pdf".to_owned(),
        data: "BBB".to_owned(),
        cache_control: None,
    };
    assert_eq!(
        serde_json::to_string(&doc).unwrap(),
        r#"{"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"BBB"}}"#
    );
    let cited = ContentBlock::Text {
        text: "t".to_owned(),
        citations: Some(vec![json!({"a": 1})]),
        cache_control: None,
    };
    assert_eq!(
        serde_json::to_string(&cited).unwrap(),
        r#"{"type":"text","text":"t","citations":[{"a":1}]}"#
    );
}

#[test]
fn absent_keys_are_omitted_and_temperature_is_a_number() {
    let mut r = user_text("x");
    r.tools = None;
    r.thinking = None;
    r.context_management = None;
    r.output_config = Some(OutputConfig {
        effort: None,
        format: None,
    });
    r.diagnostics = Some(Diagnostics {
        previous_message_id: Some("msg_1".to_owned()),
    });
    r.temperature = serde_json::Number::from_f64(1.0);
    let out = s(&r);
    assert_eq!(
        out,
        r#"{"model":"claude-opus-5-5","messages":[{"role":"user","content":"x"}],"system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.311;"},{"type":"text","text":"You are Claude Code.","cache_control":{"type":"ephemeral"}},{"type":"text","text":"Be brief.","cache_control":{"type":"ephemeral"}}],"metadata":{"user_id":"u"},"max_tokens":128000,"temperature":1.0,"diagnostics":{"previous_message_id":"msg_1"},"stream":true}"#
    );
}

#[test]
fn output_config_format_follows_effort() {
    let c = OutputConfig {
        effort: Some("high".to_owned()),
        format: Some(json!({"type": "json_schema"})),
    };
    assert_eq!(
        serde_json::to_string(&c).unwrap(),
        r#"{"effort":"high","format":{"type":"json_schema"}}"#
    );
}
