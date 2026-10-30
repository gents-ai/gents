use super::*;

const OPUS_5_5: ModelTraits<'static> = ModelTraits {
    canonical: "claude-opus-5-5",
    capabilities: &[
        "effort",
        "max_effort",
        "xhigh_effort",
        "adaptive_thinking",
        "rejects_disabled_thinking",
        "mid_conv_tool_change",
        "per_turn_effort",
        "per_turn_timing",
        "context_management",
        "fast_mode",
        "lean_prompt",
        "refusal_fallback",
        "opus_5_5_prompt_bundle",
        "total_tokens_reminder",
    ],
    native_1m: true,
    thinking_supported: true,
};

const HAIKU_4_5: ModelTraits<'static> = ModelTraits {
    canonical: "claude-haiku-4-5",
    capabilities: &["context_management", "total_tokens_reminder"],
    native_1m: false,
    thinking_supported: true,
};

const MAIN_THREAD: Request = Request {
    provider: Provider::FirstParty,
    first_party_base_url: true,
    subscriber: true,
    agentic: true,
    interactive: true,
    show_thinking_summaries: false,
    thinking: true,
    long_context_suffix: false,
    cache_one_hour: true,
    output_format: false,
    experimental_betas_disabled: false,
};

#[test]
fn main_thread_subscriber_matches_claude_code() {
    assert_eq!(
        header_value(&select(&OPUS_5_5, &MAIN_THREAD)),
        "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,\
         context-management-2025-06-27,prompt-caching-scope-2026-01-05,\
         per-turn-control-2026-07-01,mid-conversation-tool-changes-2026-07-01,\
         thinking-token-count-2026-05-13,effort-2025-11-24,\
         thinking-binding-controls-2026-08-01,thinking-display-updates-2026-08-18,\
         extended-cache-ttl-2025-04-11,cache-diagnosis-2026-04-07"
    );
}

#[test]
fn non_interactive_never_adds_redacted_thinking() {
    let request = Request {
        interactive: false,
        ..MAIN_THREAD
    };
    assert_eq!(select(&OPUS_5_5, &request), select(&OPUS_5_5, &MAIN_THREAD));
}

#[test]
fn subagent_keeps_the_five_minute_cache() {
    let request = Request {
        cache_one_hour: false,
        ..MAIN_THREAD
    };
    let betas = select(&OPUS_5_5, &request);
    assert!(!betas.contains(&EXTENDED_CACHE_TTL));
    assert!(betas.contains(&CACHE_DIAGNOSIS));
}

#[test]
fn thinking_summaries_keep_redaction_off_and_skip_display_updates() {
    let request = Request {
        show_thinking_summaries: true,
        ..MAIN_THREAD
    };
    let betas = select(&OPUS_5_5, &request);
    assert!(!betas.contains(&REDACT_THINKING));
    assert!(!betas.contains(&THINKING_DISPLAY_UPDATES));
}

#[test]
fn thinking_off_keeps_redacted_thinking() {
    let request = Request {
        thinking: false,
        ..MAIN_THREAD
    };
    let betas = select(&OPUS_5_5, &request);
    assert!(betas.contains(&REDACT_THINKING));
    assert!(!betas.contains(&THINKING_BINDING_CONTROLS));
    assert!(!betas.contains(&THINKING_DISPLAY_UPDATES));
}

#[test]
fn title_request_on_haiku() {
    let request = Request {
        agentic: false,
        thinking: false,
        cache_one_hour: false,
        output_format: true,
        ..MAIN_THREAD
    };
    assert_eq!(
        select(&HAIKU_4_5, &request),
        [
            OAUTH,
            INTERLEAVED_THINKING,
            REDACT_THINKING,
            CONTEXT_MANAGEMENT,
            PROMPT_CACHING_SCOPE,
            THINKING_TOKEN_COUNT,
            STRUCTURED_OUTPUTS,
            CACHE_DIAGNOSIS,
        ]
    );
}

#[test]
fn agentic_haiku_appends_claude_code_after_the_base_list() {
    let request = Request {
        thinking: false,
        cache_one_hour: false,
        ..MAIN_THREAD
    };
    let betas = select(&HAIKU_4_5, &request);
    let claude_code = betas.iter().position(|beta| *beta == CLAUDE_CODE).unwrap();
    let scope = betas
        .iter()
        .position(|beta| *beta == PROMPT_CACHING_SCOPE)
        .unwrap();
    assert!(claude_code > scope);
}

#[test]
fn api_key_has_no_oauth_and_custom_base_url_drops_first_party_fields() {
    let api_key = Request {
        subscriber: false,
        cache_one_hour: false,
        ..MAIN_THREAD
    };
    assert!(!select(&OPUS_5_5, &api_key).contains(&OAUTH));

    let custom = Request {
        first_party_base_url: false,
        ..MAIN_THREAD
    };
    let betas = select(&OPUS_5_5, &custom);
    for absent in [
        PROMPT_CACHING_SCOPE,
        THINKING_BINDING_CONTROLS,
        CACHE_DIAGNOSIS,
    ] {
        assert!(!betas.contains(&absent), "{absent}");
    }
}

#[test]
fn long_context_suffix_only_for_models_without_native_1m() {
    let request = Request {
        long_context_suffix: true,
        ..MAIN_THREAD
    };
    assert!(!select(&OPUS_5_5, &request).contains(&CONTEXT_1M));
    assert_eq!(select(&HAIKU_4_5, &request)[1], CONTEXT_1M);
}

#[test]
fn vertex_receives_the_third_party_subset() {
    let request = Request {
        provider: Provider::Vertex,
        subscriber: false,
        cache_one_hour: false,
        ..MAIN_THREAD
    };
    assert_eq!(
        select(&OPUS_5_5, &request),
        [CLAUDE_CODE, INTERLEAVED_THINKING, WEB_SEARCH, EFFORT]
    );
}

#[test]
fn bedrock_moves_interleaved_thinking_to_the_body() {
    let request = Request {
        provider: Provider::Bedrock,
        subscriber: false,
        cache_one_hour: false,
        ..MAIN_THREAD
    };
    let betas = select(&OPUS_5_5, &request);
    assert!(!betas.contains(&INTERLEAVED_THINKING));
    assert_eq!(betas, [CLAUDE_CODE, EFFORT]);
}

#[test]
fn experimental_betas_disabled_keeps_only_unconditional_ones() {
    let request = Request {
        experimental_betas_disabled: true,
        ..MAIN_THREAD
    };
    assert_eq!(
        select(&OPUS_5_5, &request),
        [CLAUDE_CODE, OAUTH, INTERLEAVED_THINKING, EFFORT]
    );
}

#[test]
fn no_beta_repeats() {
    for interactive in [false, true] {
        for thinking in [false, true] {
            for agentic in [false, true] {
                let request = Request {
                    interactive,
                    thinking,
                    agentic,
                    ..MAIN_THREAD
                };
                for model in [OPUS_5_5, HAIKU_4_5] {
                    let betas = select(&model, &request);
                    let mut unique = betas.clone();
                    unique.sort_unstable();
                    unique.dedup();
                    assert_eq!(unique.len(), betas.len());
                }
            }
        }
    }
}

#[test]
fn the_effort_beta_follows_the_effort_gate() {
    let opus_4_5 = ModelTraits {
        canonical: "claude-opus-4-5",
        capabilities: &["context_management", "total_tokens_reminder"],
        native_1m: false,
        thinking_supported: true,
    };
    // First party accepts effort on every model outside the exclusion list,
    // with or without the catalog capability.
    assert!(select(&opus_4_5, &MAIN_THREAD).contains(&EFFORT));
    assert!(!select(&HAIKU_4_5, &MAIN_THREAD).contains(&EFFORT));
    let vertex = Request {
        provider: Provider::Vertex,
        ..MAIN_THREAD
    };
    assert!(!select(&opus_4_5, &vertex).contains(&EFFORT));
    assert!(select(&OPUS_5_5, &vertex).contains(&EFFORT));
}
