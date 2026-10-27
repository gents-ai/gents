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
fn mcp_discovery_guidance_matches_presented_operations_model() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = snapshot["discovery_guidance_cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let tools = case["presented"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect::<Vec<_>>();
        let preamble = build_preamble(
            "Instructions",
            "worker",
            &tools,
            case["enabled"].as_bool().unwrap(),
        );
        assert_eq!(
            preamble.contains("## Tool Discovery"),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        for name in tools {
            assert!(
                preamble.contains(name),
                "direct tool {name} must remain visible"
            );
        }
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
    assert!(preamble.ends_with(SOURCE_HEADER_GUIDANCE));
}
