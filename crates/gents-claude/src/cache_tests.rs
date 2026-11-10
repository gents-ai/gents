use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};

use super::*;
use crate::wire::RawObject;

fn cc() -> CacheControl {
    CacheControl { ttl: None }
}

fn text(t: &str) -> ContentBlock {
    ContentBlock::Text {
        text: t.to_owned(),
        citations: None,
        cache_control: None,
    }
}

fn raw(kind: &str) -> ContentBlock {
    ContentBlock::Raw(RawObject::from_json(&json!({ "type": kind }).to_string()).unwrap())
}

fn block_of(kind: &str, t: Option<&str>) -> ContentBlock {
    match kind {
        "text" => text(t.unwrap_or("")),
        "thinking" => ContentBlock::Thinking {
            thinking: String::new(),
            signature: String::new(),
        },
        "redacted_thinking" => ContentBlock::RedactedThinking {
            data: String::new(),
        },
        other => raw(other),
    }
}

fn role_of(role: &str) -> Role {
    match role {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        other => panic!("unknown role {other}"),
    }
}

#[test]
fn generated_cache_cases_place_the_breakpoint() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["prompt_assembly_claude_cache_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = &case["name"];
        let mut messages: Vec<Message> = case["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                let content = &m["content"];
                let content = match content["blocks"].as_array() {
                    Some(blocks) => MessageContent::Blocks(
                        blocks
                            .iter()
                            .map(|b| block_of(b["type"].as_str().unwrap(), b["text"].as_str()))
                            .collect(),
                    ),
                    None => MessageContent::Text(content["text"].as_str().unwrap().to_owned()),
                };
                Message {
                    role: role_of(m["role"].as_str().unwrap()),
                    content,
                    output_config: None,
                }
            })
            .collect();
        place(
            &mut messages,
            case["caching"].as_bool().unwrap(),
            case["skip_write"].as_bool().unwrap(),
            &cc(),
        );
        let placed = case["placed"].as_array().unwrap();
        assert_eq!(messages.len(), placed.len(), "{name}");
        for (got, want) in messages.iter().zip(placed) {
            assert_eq!(got.role, role_of(want["role"].as_str().unwrap()), "{name}");
            let marker = want["marker"].as_u64().map(|m| m as usize);
            match &got.content {
                MessageContent::Text(t) => {
                    assert_eq!(Some(t.as_str()), want["text"].as_str(), "{name}");
                    assert!(want["blocks"].is_null() && marker.is_none(), "{name}");
                }
                MessageContent::Blocks(blocks) => {
                    let want_blocks = want["blocks"].as_array().unwrap();
                    assert_eq!(blocks.len(), want_blocks.len(), "{name}");
                    for (b, w) in blocks.iter().zip(want_blocks) {
                        assert_eq!(b.kind(), w["type"].as_str().unwrap(), "{name}");
                        let t = match b {
                            ContentBlock::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        };
                        assert_eq!(t, w["text"].as_str(), "{name}");
                    }
                    let got_marker = blocks.iter().position(|b| b.cache_control().is_some());
                    assert_eq!(got_marker, marker, "{name}");
                }
            }
        }
        let want_ttl = if case["ttl"] == "1h" {
            CacheTtl::OneHour
        } else {
            CacheTtl::FiveMinutes
        };
        let got = ttl(
            case["subscriber"].as_bool().unwrap(),
            case["main_thread"].as_bool().unwrap(),
            case["using_overage"].as_bool().unwrap(),
        );
        assert_eq!(got, want_ttl, "{name}");
    }
}

#[test]
fn ttl_truth_table() {
    for bits in 0..8u8 {
        let (s, m, o) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        let want = if s && m && !o {
            CacheTtl::OneHour
        } else {
            CacheTtl::FiveMinutes
        };
        assert_eq!(ttl(s, m, o), want);
    }
}

#[test]
fn js_whitespace_set() {
    for c in [
        '\t', '\n', '\u{b}', '\u{c}', '\r', ' ', '\u{a0}', '\u{1680}', '\u{2000}', '\u{200a}',
        '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
    ] {
        assert!(js_whitespace(c), "{c:?}");
    }
    for c in [
        '\u{85}', '\u{200b}', '\u{180e}', 'a', '\u{1fff}', '\u{200b}', '\u{e000}',
    ] {
        assert!(!js_whitespace(c), "{c:?}");
    }
    assert!(is_blank("") && is_blank(" \u{feff}\n") && !is_blank(" x "));
}

#[test]
fn normalize_drops_blank_text_only() {
    let mut blocks = vec![text(" "), raw("tool_result"), text(""), text("x")];
    normalize_user_blocks(&mut blocks);
    assert_eq!(blocks, vec![raw("tool_result"), text("x")]);
    let mut blocks = vec![text(" "), text("\n")];
    normalize_user_blocks(&mut blocks);
    assert_eq!(blocks, vec![text("(no content)")]);
    let mut blocks: Vec<ContentBlock> = vec![];
    normalize_user_blocks(&mut blocks);
    assert!(blocks.is_empty());
}

fn msg(role: Role, blocks: Vec<ContentBlock>) -> Message {
    Message {
        role,
        content: MessageContent::Blocks(blocks),
        output_config: None,
    }
}

#[test]
fn marks_last_eligible_and_skips_unstampable_assistant() {
    let mut m = vec![
        msg(Role::User, vec![text("a")]),
        msg(
            Role::Assistant,
            vec![
                text("b"),
                ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: String::new(),
                },
            ],
        ),
    ];
    place(&mut m, true, false, &cc());
    let MessageContent::Blocks(b) = &m[0].content else {
        panic!()
    };
    assert!(b[0].cache_control().is_some());
    let MessageContent::Blocks(b) = &m[1].content else {
        panic!()
    };
    assert!(b.iter().all(|b| b.cache_control().is_none()));
}

#[test]
fn skip_write_picks_the_previous_eligible() {
    let mut m = vec![
        Message {
            role: Role::User,
            content: MessageContent::Text("a".to_owned()),
            output_config: None,
        },
        Message {
            role: Role::Assistant,
            content: MessageContent::Text("b".to_owned()),
            output_config: None,
        },
        Message {
            role: Role::User,
            content: MessageContent::Text("c".to_owned()),
            output_config: None,
        },
    ];
    place(&mut m, true, true, &cc());
    assert!(matches!(&m[0].content, MessageContent::Text(_)));
    assert!(matches!(&m[1].content, MessageContent::Blocks(b) if b[0].cache_control().is_some()));
    assert!(matches!(&m[2].content, MessageContent::Text(_)));
}

#[test]
fn caching_off_still_normalizes_users() {
    let mut m = vec![msg(Role::User, vec![text(" ")])];
    place(&mut m, false, false, &cc());
    assert_eq!(
        m[0].content,
        MessageContent::Blocks(vec![text("(no content)")])
    );
}

fn random_block(rng: &mut StdRng) -> ContentBlock {
    match rng.random_range(0..8) {
        0 => text(""),
        1 => text(" \u{a0}"),
        2 => text("hello"),
        3 => block_of("thinking", None),
        4 => block_of("redacted_thinking", None),
        5 => raw("fallback"),
        6 => raw("tool_use"),
        _ => raw("tool_result"),
    }
}

#[test]
fn property_single_valid_marker() {
    let mut rng = StdRng::seed_from_u64(0x5eed_cafe);
    for _ in 0..2000 {
        let n = rng.random_range(0..6);
        let mut messages: Vec<Message> = (0..n)
            .map(|_| {
                let role = match rng.random_range(0..5) {
                    0 | 1 => Role::User,
                    2 | 3 => Role::Assistant,
                    _ => Role::System,
                };
                let content = if rng.random_bool(0.25) {
                    // Lean marks a string body as is, even when empty, so strings stay non-empty here.
                    MessageContent::Text("s".to_owned())
                } else {
                    MessageContent::Blocks(
                        (0..rng.random_range(0..4))
                            .map(|_| random_block(&mut rng))
                            .collect(),
                    )
                };
                Message {
                    role,
                    content,
                    output_config: None,
                }
            })
            .collect();
        place(
            &mut messages,
            rng.random_bool(0.8),
            rng.random_bool(0.3),
            &cc(),
        );
        let mut marked = 0;
        for m in &messages {
            if let MessageContent::Blocks(blocks) = &m.content {
                for b in blocks {
                    if b.cache_control().is_some() {
                        marked += 1;
                        assert_ne!(m.role, Role::System);
                        if m.role == Role::Assistant {
                            assert!(!matches!(
                                b,
                                ContentBlock::Thinking { .. }
                                    | ContentBlock::RedactedThinking { .. }
                            ));
                            assert_ne!(b.kind(), "fallback");
                            assert!(
                                !matches!(b, ContentBlock::Text { text, .. } if text.is_empty())
                            );
                        }
                    }
                }
                if m.role == Role::User {
                    let blank = blocks
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::Text { text, .. } if is_blank(text)))
                        .count();
                    assert_eq!(blank, 0);
                }
            }
        }
        assert!(marked <= 1);
    }
}
