//! Pure-function transcript renderer for read_subagent.

use crate::background_tools::r4c_args::PER_TOOL_RESULT_SNIPPET_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageRoleView {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageKindView {
    Ordinary {
        body: String,
    },
    AssistantWithToolCalls {
        body: String,
        tool_call_count: u32,
        bridge_call_ids: Vec<String>,
        non_bridge_tool_call_count: u32,
    },
    ToolResult {
        tool_name: String,
        body: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct MessageView {
    pub(crate) sequence: u64,
    pub(crate) role: MessageRoleView,
    pub(crate) kind: MessageKindView,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RenderOptions {
    pub(crate) include_user_messages: bool,
    pub(crate) include_tool_results: bool,
    pub(crate) limit: u32,
    pub(crate) max_chars: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct RenderOutput {
    pub(crate) transcript: String,
    pub(crate) from_sequence: u64,
    pub(crate) through_sequence: u64,
    pub(crate) next_sequence: u64,
    /// True when the token budget (or per-page block ceiling) capped this read
    /// AND at least one renderable message remains at or after `next_sequence`.
    /// This is the honest resume signal: the cursor points at exactly where the
    /// next read should continue (no gap, no overlap), never silently dropping
    /// output without flagging it.
    pub(crate) has_more: bool,
}

pub(crate) fn render_transcript(
    messages: &[MessageView],
    since_sequence: u64,
    opts: RenderOptions,
) -> RenderOutput {
    let mut transcript = String::new();
    let mut first_included: Option<u64> = None;
    let mut last_included = since_sequence;
    let mut included_count = 0;
    let mut capped = false;

    for msg in messages {
        if msg.sequence < since_sequence {
            continue;
        }
        if !opts.include_user_messages
            && msg.role == MessageRoleView::User
            && !matches!(&msg.kind, MessageKindView::ToolResult { .. })
        {
            continue;
        }

        let Some(block) = render_block(msg, opts) else {
            continue;
        };
        let projected_len = transcript.len() + block.len() + usize::from(!transcript.is_empty());
        if included_count + 1 > opts.limit || projected_len > opts.max_chars as usize {
            // Always emit the first eligible block on this page even when it
            // exceeds the budget: skipping it silently violates the
            // content-honest contract and produces a non-advancing cursor.
            if included_count == 0 {
                // Force-emit the oversized block so the page always makes
                // progress — but TRUNCATE it to respect the budget (context-honest).
                // We snap to a char boundary, append a marker, and advance the
                // cursor so paging never re-serves this message.
                let separator_overhead = usize::from(!transcript.is_empty()); // 1 newline or 0
                let budget = (opts.max_chars as usize).saturating_sub(separator_overhead);
                let emitted = truncate_to_budget(&block, budget);
                if !transcript.is_empty() {
                    transcript.push('\n');
                }
                transcript.push_str(&emitted);
                first_included.get_or_insert(msg.sequence);
                last_included = msg.sequence;
            }
            capped = true;
            break;
        }

        if !transcript.is_empty() {
            transcript.push('\n');
        }
        transcript.push_str(&block);
        included_count += 1;
        first_included.get_or_insert(msg.sequence);
        last_included = msg.sequence;
    }

    let from_sequence = first_included.unwrap_or(since_sequence);
    let next_sequence = last_included
        .saturating_add(1)
        .max(since_sequence.saturating_add(1));

    // Honest `has_more`: only true when capping left a renderable message at or
    // after the resume cursor. A read that ran out of budget exactly on the last
    // message reports `has_more = false`.
    let has_more = capped
        && messages
            .iter()
            .any(|msg| msg.sequence >= next_sequence && would_render(msg, opts));

    RenderOutput {
        transcript,
        from_sequence,
        through_sequence: last_included,
        next_sequence,
        has_more,
    }
}

/// Truncate `text` to at most `budget` chars (char-boundary-safe) and, when
/// truncation was needed, append a human-readable marker of the form
/// `…[truncated: showed N of M chars]`.
///
/// The prefix is exactly `budget` chars (or fewer if the text is shorter).
/// When truncation occurs the total result is `budget + marker_len` chars.
/// The marker is a small constant overhead (~40-60 chars); the budget cap
/// ensures the main body content never exceeds `budget` chars.
fn truncate_to_budget(text: &str, budget: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= budget {
        return text.to_string();
    }
    // Take exactly `budget` chars — char-boundary-safe via the chars iterator.
    let keep = budget.min(total_chars);
    let prefix: String = text.chars().take(keep).collect();
    let shown = prefix.chars().count();
    // Append the marker as a small constant overhead on top of the budget.
    format!("{prefix}\u{2026}[truncated: showed {shown} of {total_chars} chars]")
}

/// Whether a message would produce a rendered block under the given options
/// (mirrors the filter + `render_block` decision without materializing output).
fn would_render(msg: &MessageView, opts: RenderOptions) -> bool {
    if !opts.include_user_messages
        && msg.role == MessageRoleView::User
        && !matches!(&msg.kind, MessageKindView::ToolResult { .. })
    {
        return false;
    }
    render_block(msg, opts).is_some()
}

fn render_block(msg: &MessageView, opts: RenderOptions) -> Option<String> {
    match (&msg.role, &msg.kind) {
        (MessageRoleView::User, MessageKindView::Ordinary { body }) => {
            Some(format!("[user seq={}]\n{}", msg.sequence, body))
        }
        (MessageRoleView::Assistant, MessageKindView::Ordinary { body }) => {
            Some(format!("[assistant seq={}]\n{}", msg.sequence, body))
        }
        (
            MessageRoleView::Assistant,
            MessageKindView::AssistantWithToolCalls {
                body,
                non_bridge_tool_call_count,
                ..
            },
        ) => {
            if *non_bridge_tool_call_count == 0 {
                Some(format!("[assistant seq={}]\n{}", msg.sequence, body))
            } else {
                Some(format!(
                    "[assistant seq={} tool_calls={}]\n{}",
                    msg.sequence, non_bridge_tool_call_count, body
                ))
            }
        }
        (MessageRoleView::User, MessageKindView::ToolResult { tool_name, body }) => {
            if !opts.include_tool_results {
                None
            } else {
                let snippet: String = body.chars().take(PER_TOOL_RESULT_SNIPPET_BYTES).collect();
                Some(format!(
                    "[tool-result seq={} tool={}]\n{}",
                    msg.sequence, tool_name, snippet
                ))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTS_DEFAULT: RenderOptions = RenderOptions {
        include_user_messages: false,
        include_tool_results: false,
        limit: 20,
        max_chars: 6000,
    };

    fn assistant(sequence: u64, body: &str) -> MessageView {
        MessageView {
            sequence,
            role: MessageRoleView::Assistant,
            kind: MessageKindView::Ordinary {
                body: body.to_string(),
            },
        }
    }

    fn user(sequence: u64, body: &str) -> MessageView {
        MessageView {
            sequence,
            role: MessageRoleView::User,
            kind: MessageKindView::Ordinary {
                body: body.to_string(),
            },
        }
    }

    fn assistant_with_tool_calls(
        sequence: u64,
        body: &str,
        bridge_call_ids: Vec<&str>,
        non_bridge_tool_call_count: u32,
    ) -> MessageView {
        MessageView {
            sequence,
            role: MessageRoleView::Assistant,
            kind: MessageKindView::AssistantWithToolCalls {
                body: body.to_string(),
                tool_call_count: bridge_call_ids.len() as u32 + non_bridge_tool_call_count,
                bridge_call_ids: bridge_call_ids
                    .into_iter()
                    .map(ToString::to_string)
                    .collect(),
                non_bridge_tool_call_count,
            },
        }
    }

    fn tool_result(sequence: u64, tool: &str, body: &str) -> MessageView {
        MessageView {
            sequence,
            role: MessageRoleView::User,
            kind: MessageKindView::ToolResult {
                tool_name: tool.to_string(),
                body: body.to_string(),
            },
        }
    }

    #[test]
    fn assistant_only_default() {
        let msgs = vec![
            assistant(1, "hello"),
            user(2, "ignored"),
            assistant(3, "world"),
        ];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(out.transcript.contains("[assistant seq=1]"));
        assert!(out.transcript.contains("[assistant seq=3]"));
        assert!(!out.transcript.contains("[user"));
        assert_eq!(out.from_sequence, 1);
        assert_eq!(out.through_sequence, 3);
        assert_eq!(out.next_sequence, 4);
        assert!(!out.has_more);
    }

    #[test]
    fn include_user_messages_when_opted_in() {
        let msgs = vec![
            assistant(1, "hello"),
            user(2, "real input"),
            assistant(3, "ok"),
        ];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions {
                include_user_messages: true,
                ..OPTS_DEFAULT
            },
        );
        assert!(out.transcript.contains("[user seq=2]"));
        assert!(out.transcript.contains("real input"));
    }

    #[test]
    fn bridge_only_assistant_renders_plain() {
        let msgs = vec![assistant_with_tool_calls(
            5,
            "spawning child",
            vec!["bridge-1"],
            0,
        )];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(out.transcript.contains("[assistant seq=5]"));
        assert!(!out.transcript.contains("tool_calls="));
        assert!(!out.transcript.contains("bridge-1"));
    }

    #[test]
    fn non_bridge_tool_calls_render_count_suffix() {
        let msgs = vec![assistant_with_tool_calls(
            5,
            "using visible tool",
            vec!["bridge-1"],
            2,
        )];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(out.transcript.contains("[assistant seq=5 tool_calls=2]"));
        assert!(!out.transcript.contains("bridge-1"));
    }

    #[test]
    fn tool_result_hidden_by_default() {
        let msgs = vec![assistant(1, "hi"), tool_result(2, "bash", "stdout")];
        let out = render_transcript(&msgs, 0, OPTS_DEFAULT);
        assert!(!out.transcript.contains("[tool-result"));
    }

    #[test]
    fn tool_result_snippet_capped() {
        let big_body = "x".repeat(1024);
        let msgs = vec![tool_result(1, "bash", &big_body)];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions {
                include_tool_results: true,
                ..OPTS_DEFAULT
            },
        );
        assert!(out.transcript.contains("[tool-result seq=1 tool=bash]"));
        let snippet_len = out
            .transcript
            .split("[tool-result seq=1 tool=bash]\n")
            .nth(1)
            .expect("snippet body")
            .len();
        assert!(snippet_len <= PER_TOOL_RESULT_SNIPPET_BYTES);
    }

    #[test]
    fn since_sequence_skips_earlier() {
        let msgs = vec![assistant(1, "a"), assistant(2, "b"), assistant(3, "c")];
        let out = render_transcript(&msgs, 2, OPTS_DEFAULT);
        assert!(!out.transcript.contains("[assistant seq=1]"));
        assert!(out.transcript.contains("[assistant seq=2]"));
        assert_eq!(out.from_sequence, 2);
    }

    #[test]
    fn has_more_when_limit_hit_and_more_remain() {
        let msgs = vec![assistant(1, "a"), assistant(2, "b"), assistant(3, "c")];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions {
                limit: 2,
                ..OPTS_DEFAULT
            },
        );
        assert!(out.has_more);
        assert_eq!(out.through_sequence, 2);
        assert_eq!(out.next_sequence, 3);
    }

    #[test]
    fn has_more_when_max_chars_hit() {
        let long = "x".repeat(200);
        let msgs = vec![
            assistant(1, &long),
            assistant(2, &long),
            assistant(3, &long),
        ];
        let out = render_transcript(
            &msgs,
            0,
            RenderOptions {
                max_chars: 250,
                ..OPTS_DEFAULT
            },
        );
        assert!(out.has_more);
        assert!(out.transcript.len() <= 250);
    }

    #[test]
    fn no_has_more_when_budget_exhausted_on_last_message() {
        // Two messages, budget fits exactly one. After page 1 the cursor points
        // past message 1; message 2 still renders, so has_more is true. After
        // page 2 (resuming at the cursor) nothing renderable remains: has_more
        // must be false even though page 2 itself was budget-tight.
        let long = "x".repeat(80);
        let msgs = vec![assistant(1, &long), assistant(2, &long)];
        let opts = RenderOptions {
            max_chars: 100,
            ..OPTS_DEFAULT
        };
        let page1 = render_transcript(&msgs, 0, opts);
        assert!(page1.has_more);
        assert_eq!(page1.through_sequence, 1);
        let page2 = render_transcript(&msgs, page1.next_sequence, opts);
        assert_eq!(page2.through_sequence, 2);
        assert!(!page2.has_more, "no renderable message past the cursor");
    }

    #[test]
    fn oversized_single_block_is_truncated_not_dropped_and_respects_budget() {
        // A single assistant turn whose rendered block exceeds max_chars must:
        //   (a) return a NON-EMPTY transcript containing a TRUNCATED prefix
        //   (b) include a truncation MARKER ("…[truncated: showed N of M chars]")
        //   (c) NOT blow the context budget — transcript length within max_chars + small slack
        //   (d) NOT contain the full oversized body
        //   (e) advance the cursor so a follow-up read terminates honestly (has_more=false)
        let big_body = "y".repeat(500);
        let msgs = vec![assistant(1, &big_body), assistant(2, "small")];
        let opts = RenderOptions {
            max_chars: 50, // far smaller than the 500-char block
            ..OPTS_DEFAULT
        };
        // Page 1: the oversized block must be TRUNCATED and still emitted.
        let page1 = render_transcript(&msgs, 0, opts);

        // (a) Non-empty; contains the header and a prefix of the body.
        assert!(
            !page1.transcript.is_empty(),
            "page1 must not be empty for an oversized first block"
        );
        assert!(
            page1.transcript.contains("[assistant seq=1]"),
            "oversized first block header must be present: {:?}",
            page1.transcript
        );

        // (b) Truncation marker is present.
        assert!(
            page1.transcript.contains("[truncated:"),
            "truncation marker must be present: {:?}",
            page1.transcript
        );

        // (c) Budget respected — allow a small marker slack (80 chars above max_chars).
        let marker_slack = 80usize;
        assert!(
            page1.transcript.len() <= opts.max_chars as usize + marker_slack,
            "transcript ({} chars) must stay within budget ({} + {} slack): {:?}",
            page1.transcript.len(),
            opts.max_chars,
            marker_slack,
            page1.transcript
        );

        // (d) Full oversized body must NOT appear.
        assert!(
            !page1.transcript.contains(&big_body),
            "full oversized body must NOT be present (budget violation): transcript len={}",
            page1.transcript.len()
        );

        // (e) Cursor advanced.
        assert!(
            page1.through_sequence >= 1,
            "through_sequence must cover sequence 1"
        );
        assert!(
            page1.next_sequence >= 2,
            "next_sequence must point past the emitted block"
        );

        // Page 2: resume cursor picks up the second (small) message and terminates.
        let page2 = render_transcript(&msgs, page1.next_sequence, opts);
        assert!(
            page2.transcript.contains("[assistant seq=2]"),
            "page 2 must emit the second message"
        );
        assert!(!page2.has_more, "no more messages after page 2");
    }

    #[test]
    fn paging_is_gap_free_across_token_budget() {
        // Ten messages, small budget forces several pages. Walking the cursor
        // must visit every message exactly once with no gap or overlap.
        let msgs: Vec<MessageView> = (1..=10)
            .map(|seq| assistant(seq, &format!("turn {seq}")))
            .collect();
        let opts = RenderOptions {
            max_chars: 40,
            ..OPTS_DEFAULT
        };
        let mut cursor = 0u64;
        let mut seen = Vec::new();
        loop {
            let page = render_transcript(&msgs, cursor, opts);
            for seq in page.from_sequence..=page.through_sequence {
                if page.transcript.contains(&format!("turn {seq}")) {
                    seen.push(seq);
                }
            }
            if !page.has_more {
                break;
            }
            assert!(page.next_sequence > cursor, "cursor must advance");
            cursor = page.next_sequence;
        }
        assert_eq!(seen, (1..=10).collect::<Vec<_>>());
    }
}
