use super::*;
use rig::completion::message::AssistantContent;

fn test_builder(system_prompt: &str, behavior_name: &str) -> LayeredPromptBuilder {
    LayeredPromptBuilder::for_behavior(
        system_prompt,
        behavior_name,
        &["list_files", "read_file", "bash"],
        true,
        100_000,
        8_192,
    )
}

fn user_msg(text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: text.to_string(),
        })),
    }
}

fn assistant_msg(text: &str) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::Text(Text {
            text: text.to_string(),
        })),
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
fn preamble_is_frozen() {
    let builder = test_builder("System prompt v1.", "test");

    assert_eq!(builder.preamble(), builder.preamble());
    assert!(builder.preamble().contains("System prompt v1."));
}

#[tokio::test]
async fn build_without_summaries() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![user_msg("hello"), assistant_msg("hi")];
    let prompt = builder.build(&messages, &[]).await.unwrap();

    assert_eq!(prompt.messages.len(), 2);
    assert!(prompt.estimated_tokens > 0);
    assert!(prompt.preamble.contains("Be helpful."));
}

#[tokio::test]
async fn build_with_summaries_prepends() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![user_msg("what were we discussing?")];
    let summaries = vec!["We discussed project architecture.".to_string()];
    let prompt = builder.build(&messages, &summaries).await.unwrap();

    assert_eq!(prompt.messages.len(), 2);

    if let Message::User { content } = &prompt.messages[0] {
        if let UserContent::Text(t) = content.first_ref() {
            assert!(t.text.contains("<system-reminder>"));
            assert!(t.text.contains("project architecture"));
        } else {
            panic!("expected text");
        }
    } else {
        panic!("expected user message");
    }
}

#[test]
fn system_reminder_format() {
    let msg = LayeredPromptBuilder::system_reminder("The time is 3pm.");
    if let Message::User { content } = &msg {
        if let UserContent::Text(t) = content.first_ref() {
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
fn message_budget_accounts_for_preamble_and_output() {
    let builder = LayeredPromptBuilder::for_behavior(
        &"x".repeat(4000),
        "general",
        &["list_files", "read_file", "bash"],
        true,
        10_000,
        2_000,
    );

    let budget = builder.message_budget();
    assert!(budget < 10000);
    assert!(budget > 5000);
}

#[test]
fn would_exceed_budget_short_messages() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![user_msg("hi")];
    assert!(!builder.would_exceed_budget(&messages));
}

#[test]
fn would_exceed_budget_long_messages() {
    let builder = LayeredPromptBuilder::for_behavior(
        "Be helpful.",
        "general",
        &["list_files", "read_file", "bash"],
        true,
        100,
        50,
    );

    let big = user_msg(&"x".repeat(10000));
    assert!(builder.would_exceed_budget(&[big]));
}
