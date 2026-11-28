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
