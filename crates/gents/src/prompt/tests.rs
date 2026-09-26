use super::*;
use crate::llm::message::AssistantContent;
use crate::test_support::first_content;
use gents_loop::claude_messages_body::ReplayTag;
use gents_protocol::output::OutputSource;
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

fn test_builder(system_prompt: &str, behavior_name: &str) -> LayeredPromptBuilder {
    LayeredPromptBuilder::for_behavior(
        system_prompt,
        behavior_name,
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

fn assistant_msg(text: &str) -> Message {
    Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

#[test]
fn preamble_combines_prompt_and_behavior_name() {
    let preamble = build_preamble(
        "You are a helpful assistant.",
        "research",
        &["list_files"],
        true,
    );
    assert!(preamble.contains("You are a helpful assistant."));
    assert!(preamble.contains("You are the research agent."));
    assert!(preamble.contains("## Tool Discovery"));
    assert!(preamble.contains("discover_tools"));
    assert!(preamble.contains("describe_tool"));
    assert!(preamble.contains("call_tool"));
    assert!(preamble.contains("native direct tools"));
    assert!(preamble.contains("synthetic `native` service"));
    assert!(preamble.contains("list_files"));
}

#[test]
fn preamble_handles_empty_system_prompt() {
    let preamble = build_preamble("", "general", &["bash"], true);
    assert!(preamble.contains("You are the general agent."));
    assert!(preamble.contains("## Tool Discovery"));
}

#[test]
fn preamble_handles_empty_behavior_name() {
    let preamble = build_preamble("Be helpful.", "", &[], true);
    assert!(preamble.contains("Be helpful."));
    assert!(preamble.contains("## Tool Discovery"));
}

#[test]
fn preamble_strips_title_generation_suffix() {
    let preamble = build_preamble(
        "You are a policy agent.\n\nGenerate concise conversation titles. Return only a lowercase hyphenated 3-5 word title. Never call tools. Never explain.",
        "operator",
        &["bash"],
        true,
    );
    assert!(preamble.contains("You are a policy agent."));
    assert!(!preamble.contains("Generate concise conversation titles."));
    assert!(!preamble.contains("Never call tools. Never explain."));
}

#[test]
fn preamble_is_frozen() {
    let builder = test_builder("System prompt v1.", "test");

    assert_eq!(builder.preamble(), builder.preamble());
    assert!(builder.preamble().contains("System prompt v1."));
}

#[tokio::test]
async fn build_without_summaries() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![
        TaggedMessage::unassociated(user_msg("hello")),
        TaggedMessage::unassociated(assistant_msg("hi")),
    ];
    let prompt = builder.build(&messages, &[]).await.unwrap();

    assert_eq!(prompt.messages.len(), 2);
    assert!(prompt.preamble.contains("Be helpful."));
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
fn system_reminder_format() {
    let msg = LayeredPromptBuilder::system_reminder("The time is 3pm.");
    if let Message::User { content } = &msg {
        if let UserContent::Text(t) = first_content(content) {
            assert!(t.text.starts_with("<system-reminder>"));
            assert!(t.text.ends_with("</system-reminder>"));
            assert!(t.text.contains("The time is 3pm."));
        } else {
            panic!("expected text");
        }
    } else {
        panic!("expected user message");
    }
}

#[test]
fn preamble_lists_allowed_subagent_targets() {
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
        &["create_session"],
        false,
        &targets,
    );
    assert!(
        preamble.contains("code-reviewer"),
        "preamble should contain target id 'code-reviewer'"
    );
    assert!(
        preamble.contains("Reviews code for correctness and style."),
        "preamble should contain code-reviewer description"
    );
    assert!(
        preamble.contains("data-analyst"),
        "preamble should contain target id 'data-analyst'"
    );
    assert!(
        preamble.contains("Analyzes datasets and produces summaries."),
        "preamble should contain data-analyst description"
    );
    assert!(
        preamble.contains("create_session"),
        "preamble should reference the create_session tool"
    );
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
    // Should not contain any subagent section heading
    assert!(
        !preamble.contains("## Sub-Agents"),
        "preamble should have no subagent section when targets is empty"
    );
    assert!(
        !preamble.contains("create_session"),
        "preamble should not mention create_session when there are no targets"
    );
}
