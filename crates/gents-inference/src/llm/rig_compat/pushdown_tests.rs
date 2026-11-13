use crate::llm::rig_compat::classify_completion_error;
use gents_loop::completion_retry::{failure_class, FailureClass};

fn provider_stream_malformed(after_preview: bool) -> rig::completion::CompletionError {
    use crate::clients::claude_messages::{parse_messages_sse, MessagesSseState};
    if !after_preview {
        let sse = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n";
        return parse_messages_sse(sse, &Default::default())
            .expect_err("provider truncation must reach the Rig boundary");
    }
    let mut parser = MessagesSseState::new(Default::default());
    let preview_sse = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"partial\"}}\n\n";
    let mut saw_preview = false;
    for line in preview_sse.lines() {
        for event in parser.push_line(line).expect("provider preview") {
            saw_preview |= matches!(
                event,
                rig::streaming::RawStreamingChoice::ReasoningDelta { .. }
            );
        }
    }
    assert!(
        saw_preview,
        "the after-preview origin needs a real preview item"
    );
    parser
        .finish()
        .expect_err("provider truncated after preview")
}

fn origin_error(origin: &str, after_preview: bool) -> rig::agent::StreamingError {
    let completion = match origin {
        "local_request_build" => rig::completion::CompletionError::RequestError(Box::new(
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "malformed local request"),
        )),
        "retryable_transport" => {
            rig::completion::CompletionError::ProviderError("connection reset".into())
        }
        "provider_stream_malformed" => provider_stream_malformed(after_preview),
        "provider_account_limited" => rig::completion::CompletionError::ProviderError(
            gents_loop::provider_limit::ProviderLimitHeaders {
                usage_exhausted: true,
                account_rotated: true,
                ..Default::default()
            }
            .annotate("Claude Messages HTTP 429 Too Many Requests (request-id -) body=usage limit"),
        ),
        "provider_reasoning_rejected" => rig::completion::CompletionError::ProviderError(
            "400 invalid_request_error: messages.5.content.0: Invalid `signature` in \
             `thinking` block. The block is bound to a different conversation."
                .into(),
        ),
        other => panic!("unknown modeled failure origin {other}"),
    };
    rig::agent::StreamingError::Completion(completion)
}

fn class_name(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Transport => "transport",
        FailureClass::ParseBadRequest => "parse_bad_request",
        FailureClass::ReasoningRejected => "reasoning_rejected",
        FailureClass::ContextOverflow => "context_overflow",
        FailureClass::AccountLimited => "account_limited",
        FailureClass::Permanent => "permanent",
    }
}

#[test]
fn provider_failure_origins_classify_as_the_lean_model() {
    gents_lean_vocab::families::completion_retry_contracts_origin_class::replay::<String, String>(
        |input| {
            let classified = classify_completion_error(&origin_error(
                &input.failure_origin,
                input.after_preview,
            ));
            gents_lean_vocab::families::completion_retry_contracts_origin_class::OriginOutput {
                classified_failure: class_name(failure_class(&classified)).to_string(),
            }
        },
    );
}
