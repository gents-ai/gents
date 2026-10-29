//! Prompt-cache placement on messages: user content normalization, the single
//! message breakpoint and the cache TTL (`dsn`, `cfr`, `bcr`, `wcr`, `Gfn`,
//! `EXe`, `Tbe` in `chunk-ps7vnt4g.js`). Lean: `PromptAssembly.ClaudeCache`.

use crate::wire::{CacheControl, CacheTtl, ContentBlock, Message, MessageContent, Role};

const NO_CONTENT: &str = "(no content)";

/// Picks the cache TTL (Claude Code `Tbe`): a subscriber's main-thread request not
/// drawing paid overage caches for an hour, everything else for five minutes.
pub fn ttl(subscriber: bool, main_thread: bool, using_overage: bool) -> CacheTtl {
    if subscriber && main_thread && !using_overage {
        CacheTtl::OneHour
    } else {
        CacheTtl::FiveMinutes
    }
}

/// ECMAScript `String.prototype.trim` whitespace: WhiteSpace and LineTerminator.
pub fn js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{D}'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// Whether the text is empty or only JavaScript whitespace.
pub fn is_blank(text: &str) -> bool {
    text.chars().all(js_whitespace)
}

fn blank_text(block: &ContentBlock) -> bool {
    matches!(block, ContentBlock::Text { text, .. } if is_blank(text))
}

/// Drops blank text blocks from user content (Claude Code `Gfn`).
///
/// Content left empty by the removal becomes the single text `(no content)`;
/// content with no blank text block is unchanged.
pub fn normalize_user_blocks(blocks: &mut Vec<ContentBlock>) {
    if !blocks.iter().any(blank_text) {
        return;
    }
    blocks.retain(|b| !blank_text(b));
    if blocks.is_empty() {
        blocks.push(text_block(NO_CONTENT.to_owned(), None));
    }
}

fn text_block(text: String, cache_control: Option<CacheControl>) -> ContentBlock {
    ContentBlock::Text {
        text,
        citations: None,
        cache_control,
    }
}

/// Whether a block can carry a breakpoint (Claude Code `EXe`).
fn stampable(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::Text { text, .. } => !text.is_empty(),
        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => false,
        other => other.kind() != "fallback",
    }
}

/// Whether the message may hold the breakpoint (Claude Code `lsn`).
fn eligible(message: &Message) -> bool {
    match (message.role, &message.content) {
        (Role::User, _) | (Role::Assistant, MessageContent::Text(_)) => true,
        (Role::Assistant, MessageContent::Blocks(blocks)) => blocks.last().is_some_and(stampable),
    }
}

/// Normalizes user content and places the single message breakpoint (Claude Code `cfr`).
///
/// Every user message with block content is normalized, caching or not. When
/// `caching`, the last eligible message (the one before it when `skip_write`)
/// gets `cache` on its last block. Runs in O(total blocks): one backward scan
/// to pick the target, one forward pass to rewrite.
pub fn place(messages: &mut [Message], caching: bool, skip_write: bool, cache: &CacheControl) {
    let target = if caching {
        let mut eligible_rev = messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, m)| eligible(m));
        let last = eligible_rev.next();
        (if skip_write {
            eligible_rev.next()
        } else {
            last
        })
        .map(|(i, _)| i)
    } else {
        None
    };
    for (i, message) in messages.iter_mut().enumerate() {
        if message.role == Role::User {
            if let MessageContent::Blocks(blocks) = &mut message.content {
                normalize_user_blocks(blocks);
            }
        }
        if target == Some(i) {
            mark(message, cache);
        }
    }
}

fn mark(message: &mut Message, cache: &CacheControl) {
    let user = message.role == Role::User;
    match &mut message.content {
        MessageContent::Text(text) => {
            let text = std::mem::take(text);
            message.content = MessageContent::Blocks(vec![text_block(text, Some(cache.clone()))]);
        }
        MessageContent::Blocks(blocks) => {
            // Lean `placeOne`: a user turn marks its last block, an assistant turn only a stampable one.
            if let Some(last) = blocks.last_mut().filter(|b| user || stampable(b)) {
                last.set_cache_control(cache.clone());
            }
        }
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
