use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::json;

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

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

#[test]
fn cache_placement_matches_lean() {
    use gents_lean_vocab::families::claude_prompt_assembly_cache_placement as family;
    family::replay::<String>(|i| {
        let mut messages: Vec<Message> = i
            .messages
            .iter()
            .map(|m| {
                let content = match &m.content.blocks {
                    Some(blocks) => MessageContent::Blocks(
                        blocks
                            .iter()
                            .map(|b| block_of(&b.kind, b.text.as_deref()))
                            .collect(),
                    ),
                    None => MessageContent::Text(m.content.text.clone().unwrap()),
                };
                Message {
                    role: role_of(&m.role),
                    content,
                    output_config: None,
                }
            })
            .collect();
        place(&mut messages, i.caching, i.skip_write, &cc());
        messages
            .iter()
            .map(|message| {
                let role = role_name(message.role).to_owned();
                match &message.content {
                    MessageContent::Text(text) => family::CachePlaced {
                        role,
                        text: Some(text.clone()),
                        blocks: None,
                        marker: None,
                    },
                    MessageContent::Blocks(blocks) => family::CachePlaced {
                        role,
                        text: None,
                        blocks: Some(
                            blocks
                                .iter()
                                .map(|b| family::CacheBlock {
                                    kind: b.kind().to_owned(),
                                    text: match b {
                                        ContentBlock::Text { text, .. } => Some(text.clone()),
                                        _ => None,
                                    },
                                })
                                .collect(),
                        ),
                        marker: blocks
                            .iter()
                            .position(|b| b.cache_control().is_some())
                            .map(|marker| marker as u64),
                    },
                }
            })
            .collect()
    });
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

fn msg(role: Role, blocks: Vec<ContentBlock>) -> Message {
    Message {
        role,
        content: MessageContent::Blocks(blocks),
        output_config: None,
    }
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
