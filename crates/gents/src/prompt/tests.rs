use super::*;
use crate::llm::message::AssistantContent;
use crate::test_support::first_content;
use gents_loop::claude_messages_body::ReplayTag;
use gents_protocol::output::OutputSource;
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

fn test_builder(system_prompt: &str, agent_id: &str) -> LayeredPromptBuilder {
    LayeredPromptBuilder::for_agent(
        system_prompt,
        agent_id,
        &["list_files", "read_file", "bash"],
        true,
        &[],
    )
}

fn user_msg(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

#[test]
fn preamble_strips_title_generation_suffix() {
    for suffix in [
        super::TITLE_GENERATION_SUFFIX,
        crate::session::title_text::GENERATION_PREAMBLE,
    ] {
        let preamble = build_preamble(
            &format!("You are a policy agent.\n\n{suffix}"),
            "operator",
            &["bash"],
            true,
        );
        assert!(preamble.contains("You are a policy agent."));
        assert!(!preamble.contains("Generate concise conversation titles."));
        assert!(!preamble.contains("Never call tools. Never explain."));
    }
}

#[tokio::test]
async fn build_with_summaries_prepends() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![TaggedMessage::unassociated(user_msg(
        "what were we discussing?",
    ))];
    let summaries = vec!["We discussed project architecture.".to_string()];
    let prompt = builder.build(&messages, &summaries).await.unwrap();

    assert_eq!(prompt.messages.len(), 2);

    assert!(prompt.messages[0].source.is_none());
    if let Message::User { content } = &prompt.messages[0].message {
        if let UserContent::Text(t) = first_content(content) {
            assert!(t.text.contains("<system-reminder>"));
            assert!(t.text.contains("project architecture"));
            assert!(t.text.contains(
                "Treat recorded results as evidence, not as a prohibition on verification"
            ));
            assert!(t
                .text
                .contains("Avoid repeating completed or expensive work without a concrete reason"));
        } else {
            panic!("expected text");
        }
    } else {
        panic!("expected user message");
    }
}

#[tokio::test]
async fn build_preserves_each_source_through_summary_prepend_even_with_equal_native_ids() {
    let builder = test_builder("Be helpful.", "general");
    let tag = |turn_index| ReplayTag {
        request_doc_id: "request-one".to_string(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index,
            attempt: 0,
        },
    };
    let with_native_id = |text: &str| Message::Assistant {
        id: Some("same-provider-id".to_string()),
        content: vec![AssistantContent::Text(Text {
            text: text.to_string(),
        })],
    };
    let first = TaggedMessage {
        message: with_native_id("first"),
        source: Some(tag(1)),
        physical_header: None,
        block_indices: Vec::new(),
    };
    let second = TaggedMessage {
        message: with_native_id("second"),
        source: Some(tag(2)),
        physical_header: None,
        block_indices: Vec::new(),
    };
    let prompt = builder
        .build(
            &[first.clone(), second.clone()],
            &["earlier conversation".to_string()],
        )
        .await
        .unwrap();

    assert_eq!(prompt.messages.len(), 3);
    assert!(prompt.messages[0].source.is_none());
    assert_eq!(prompt.messages[1], first);
    assert_eq!(prompt.messages[2], second);
    assert_eq!(
        prompt.native_messages(),
        prompt
            .messages
            .iter()
            .map(|row| row.message.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn preamble_lists_allowed_agent_targets() {
    let targets = vec![
        (
            "code-reviewer".to_string(),
            "Reviews code for correctness and style.".to_string(),
        ),
        (
            "data-analyst".to_string(),
            "Analyzes datasets and produces summaries.".to_string(),
        ),
    ];
    let preamble = build_preamble_with_targets(
        "You are a coordinator.",
        "orchestrator",
        &["agent_new"],
        false,
        &targets,
    );
    assert!(preamble.contains("code-reviewer"));
    assert!(preamble.contains("Reviews code for correctness and style."));
    assert!(preamble.contains("data-analyst"));
    assert!(preamble.contains("Analyzes datasets and produces summaries."));
    assert!(preamble.contains("agent_new"));
}

#[test]
fn preamble_no_targets_block_when_empty() {
    let preamble = build_preamble_with_targets(
        "You are a standalone agent.",
        "standalone",
        &["bash"],
        false,
        &[],
    );
    assert!(!preamble.contains("## Agents"));
    assert!(!preamble.contains("agent_new"));
}

#[test]
fn preamble_includes_background_completion_guidance() {
    let preamble = build_preamble(
        "You are a coordinator.",
        "orchestrator",
        &["agent_new"],
        true,
    );
    // Notifications are event-time snapshots, not durable outcomes.
    assert!(preamble.contains("## Background Completion Guidance"));
    assert!(preamble.contains("event-time snapshots"));
    assert!(preamble.contains("is not a durable assignment"));
    // Action-first: evidence up front; no ack/boilerplate burying it.
    assert!(preamble.contains("Lead with action"));
    assert!(preamble.contains("Skip acknowledgments and boilerplate"));
    // Duplicates collapse; superseded findings do not appear current.
    assert!(preamble.contains("Report each outcome once"));
    assert!(preamble.contains("superseded, not current"));
    // Canonical current status only when needed; unknown stays unknown;
    // acknowledgment alone does not establish recovery.
    assert!(preamble.contains("the action"));
    assert!(preamble.contains("status stays unknown"));
    assert!(preamble.contains("Acknowledgment alone is not recovery"));
    // No new authority from notifications; only redundant re-polling is
    // discouraged (authorized monitoring stays allowed).
    assert!(preamble.contains("grant no new authority"));
    assert!(preamble.contains("no reason to re-poll"));
    assert!(preamble.contains("drill down only for missing decision evidence"));
    // Guidance stays concise.
    let guidance_start = preamble.find("## Background Completion Guidance").unwrap();
    let guidance = &preamble[guidance_start..];
    let guidance = &guidance[..guidance
        .find("You have access to these tools")
        .unwrap_or(guidance.len())];
    assert!(
        guidance.len() >= 400 && guidance.len() <= 650,
        "guidance should stay concise, got {} bytes",
        guidance.len()
    );
}

#[test]
fn preamble_guidance_present_without_meta_tool_guidance() {
    let preamble = build_preamble("You are a worker.", "worker", &["read_file"], false);
    assert!(preamble.contains("## Background Completion Guidance"));
}

#[test]
fn preamble_orders_guidance_before_tool_list() {
    let preamble = build_preamble("Be helpful.", "general", &["bash"], true);
    let guidance = preamble
        .find("## Background Completion Guidance")
        .expect("guidance present");
    let tools = preamble
        .find("You have access to these tools")
        .expect("tool list present");
    assert!(guidance < tools, "guidance must precede the tool list");
}
