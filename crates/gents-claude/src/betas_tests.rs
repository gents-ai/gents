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
        [
            CLAUDE_CODE,
            INTERLEAVED_THINKING,
            WEB_SEARCH,
            THINKING_TOKEN_COUNT,
            EFFORT,
            THINKING_DISPLAY_UPDATES
        ]
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
    assert_eq!(
        betas,
        [
            CLAUDE_CODE,
            THINKING_TOKEN_COUNT,
            EFFORT,
            THINKING_DISPLAY_UPDATES
        ]
    );
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

const PROVIDERS: [Provider; 8] = [
    Provider::FirstParty,
    Provider::AnthropicAws,
    Provider::Mantle,
    Provider::Foundry,
    Provider::Vertex,
    Provider::Bedrock,
    Provider::AnthropicGoogleCloud,
    Provider::Gateway,
];

fn traits(canonical: &'static str, capabilities: &'static [&'static str]) -> ModelTraits<'static> {
    ModelTraits {
        canonical,
        capabilities,
        native_1m: false,
        thinking_supported: !canonical.contains("claude-3-"),
    }
}

fn on(provider: Provider) -> Request {
    Request {
        provider,
        subscriber: false,
        ..MAIN_THREAD
    }
}

#[test]
fn provider_predicates_match_claude_code() {
    // (provider, upe, Gq)
    let expected = [
        (Provider::FirstParty, true, true),
        (Provider::AnthropicAws, true, true),
        (Provider::Mantle, false, true),
        (Provider::Foundry, true, true),
        (Provider::Vertex, false, false),
        (Provider::Bedrock, false, false),
        (Provider::AnthropicGoogleCloud, true, true),
        (Provider::Gateway, false, false),
    ];
    for (provider, upe, gq) in expected {
        assert_eq!(provider.first_party_like(), upe, "{provider:?} upe");
        assert_eq!(provider.capability_betas(), gq, "{provider:?} Gq");
    }
}

#[test]
fn thinking_betas_follow_upe_or_a_model_not_older_than_opus_4_7() {
    // (model, taken on a provider outside upe)
    let models = [
        ("claude-opus-4-0", false),
        ("claude-sonnet-4-5", false),
        ("claude-opus-4-6", false),
        ("claude-sonnet-4-6", false),
        ("claude-opus-4-7", true),
        ("claude-opus-4-8", true),
        ("claude-opus-5-5", true),
        ("claude-mythos-5", true),
        ("claude-3-5-haiku", false),
    ];
    for provider in PROVIDERS {
        for (canonical, outside_upe) in models {
            let model = traits(canonical, &[]);
            let request = on(provider);
            let interleaved = model.interleaved_thinking(provider);
            let expected = interleaved && (provider.first_party_like() || outside_upe);
            let betas = select(&model, &request);
            let kept = |beta| betas.contains(&beta);
            // Bedrock keeps the third-party subset, which holds both betas.
            assert_eq!(
                kept(THINKING_TOKEN_COUNT),
                expected && model.thinking_supported,
                "{provider:?} {canonical} token count"
            );
            assert_eq!(
                kept(THINKING_DISPLAY_UPDATES),
                expected,
                "{provider:?} {canonical} display"
            );
            assert_eq!(display_updates(&model, &request), expected);
        }
    }
}

#[test]
fn first_party_thinking_betas_are_unchanged() {
    for canonical in ["claude-opus-4-0", "claude-opus-4-6", "claude-opus-5-5"] {
        let model = traits(canonical, &[]);
        let betas = select(&model, &on(Provider::FirstParty));
        assert!(betas.contains(&THINKING_TOKEN_COUNT));
        assert!(betas.contains(&THINKING_DISPLAY_UPDATES));
    }
    let haiku = select(&traits("claude-haiku-4-5", &[]), &on(Provider::FirstParty));
    assert!(haiku.contains(&THINKING_TOKEN_COUNT));
}

#[test]
fn experimental_betas_off_drops_the_thinking_betas_everywhere() {
    for provider in PROVIDERS {
        let request = Request {
            experimental_betas_disabled: true,
            ..on(provider)
        };
        let betas = select(&OPUS_5_5, &request);
        assert!(!betas.contains(&THINKING_TOKEN_COUNT), "{provider:?}");
        assert!(!display_updates(&OPUS_5_5, &request), "{provider:?}");
    }
}

#[test]
fn web_search_goes_to_foundry_and_to_vertex_models_outside_claude_3() {
    for provider in PROVIDERS {
        for (canonical, vertex_takes) in [
            ("claude-opus-4-0", true),
            ("claude-opus-5-5", true),
            ("claude-3-5-haiku", false),
            ("claude-3-7-sonnet", false),
        ] {
            let betas = select(&traits(canonical, &[]), &on(provider));
            let expected = match provider {
                Provider::Foundry => true,
                Provider::Vertex => vertex_takes,
                _ => false,
            };
            assert_eq!(
                betas.contains(&WEB_SEARCH),
                expected,
                "{provider:?} {canonical}"
            );
        }
    }
}

#[test]
fn context_management_follows_the_per_provider_gate() {
    let catalog_named: &[&str] = &["context_management"];
    // (provider, catalog-named model, unnamed model, catalog-named claude-3 model, mythos-5)
    let expected = [
        (Provider::FirstParty, true, true, false, true),
        (Provider::AnthropicAws, true, true, false, true),
        (Provider::Mantle, true, true, false, true),
        (Provider::Foundry, true, true, true, true),
        (Provider::Vertex, true, false, true, true),
        (Provider::Bedrock, true, false, true, true),
        (Provider::AnthropicGoogleCloud, true, true, false, true),
        (Provider::Gateway, true, false, true, true),
    ];
    for (provider, named, unnamed, claude3, mythos) in expected {
        let gate = |canonical, caps| traits(canonical, caps).context_management(provider);
        assert_eq!(
            gate("claude-opus-4-6", catalog_named),
            named,
            "{provider:?}"
        );
        assert_eq!(gate("claude-opus-4-6", &[]), unnamed, "{provider:?}");
        assert_eq!(
            gate("claude-3-5-haiku", catalog_named),
            claude3,
            "{provider:?}"
        );
        assert_eq!(gate("claude-mythos-5", &[]), mythos, "{provider:?}");
    }
}

#[test]
fn the_context_management_beta_needs_a_capability_beta_provider_and_the_gate() {
    let unnamed = traits("claude-opus-4-6", &[]);
    for provider in PROVIDERS {
        let betas = select(&unnamed, &on(provider));
        let expected = provider.capability_betas();
        assert_eq!(
            betas.contains(&CONTEXT_MANAGEMENT),
            expected,
            "{provider:?}"
        );
    }
    // First party stays catalog-independent for non-Claude-3 models.
    let claude3 = traits("claude-3-5-haiku", &["context_management"]);
    assert!(!select(&claude3, &on(Provider::FirstParty)).contains(&CONTEXT_MANAGEMENT));
    assert!(select(&claude3, &on(Provider::Foundry)).contains(&CONTEXT_MANAGEMENT));
    let mythos = traits("claude-mythos-5", &[]);
    assert!(select(&mythos, &on(Provider::FirstParty)).contains(&CONTEXT_MANAGEMENT));
}

#[test]
fn the_effort_gate_agrees_with_the_body_for_every_provider() {
    let opus_4_6 = traits("claude-opus-4-6", &[]);
    for provider in PROVIDERS {
        let betas = select(&opus_4_6, &on(provider));
        assert_eq!(
            betas.contains(&EFFORT),
            crate::body::effort_capable("claude-opus-4-6", false, provider.capability_betas()),
            "{provider:?}"
        );
    }
}
