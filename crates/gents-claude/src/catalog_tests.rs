use super::*;

/// `(id, first-party id, window, default max_tokens, upper max_tokens, default effort)`.
#[rustfmt::skip]
#[allow(clippy::type_complexity)]
const MODELS: [(&str, &str, Option<u64>, u64, u64, Option<&str>); 22] = [
    ("claude-3-5-haiku", "claude-3-5-haiku-20241022", None, 8192, 8192, None),
    ("claude-haiku-4-5", "claude-haiku-4-5-20251001", Some(200000), 32000, 64000, None),
    ("claude-haiku-5-5", "claude-haiku-5-5", Some(1000000), 128000, 128000, Some("medium")),
    ("claude-3-5-sonnet", "claude-3-5-sonnet-20241022", None, 8192, 8192, None),
    ("claude-3-7-sonnet", "claude-3-7-sonnet-20250219", None, 32000, 64000, None),
    ("claude-sonnet-4-0", "claude-sonnet-4-20250514", Some(200000), 32000, 64000, None),
    ("claude-sonnet-4-5", "claude-sonnet-4-5-20250929", Some(200000), 32000, 64000, None),
    ("claude-sonnet-4-6", "claude-sonnet-4-6", Some(200000), 32000, 128000, None),
    ("claude-sonnet-5", "claude-sonnet-5", Some(1000000), 64000, 128000, Some("high")),
    ("claude-sonnet-5-5", "claude-sonnet-5-5", Some(1000000), 128000, 128000, Some("medium")),
    ("claude-opus-4-0", "claude-opus-4-20250514", Some(200000), 32000, 32000, None),
    ("claude-opus-4-1", "claude-opus-4-1-20250805", Some(200000), 32000, 32000, None),
    ("claude-opus-4-5", "claude-opus-4-5-20251101", Some(200000), 32000, 64000, None),
    ("claude-opus-4-6", "claude-opus-4-6", Some(200000), 64000, 128000, None),
    ("claude-opus-4-7", "claude-opus-4-7", Some(1000000), 64000, 128000, Some("xhigh")),
    ("claude-opus-4-8", "claude-opus-4-8", Some(1000000), 64000, 128000, Some("high")),
    ("claude-opus-5", "claude-opus-5", Some(1000000), 64000, 128000, Some("high")),
    ("claude-opus-5-5", "claude-opus-5-5", Some(1000000), 128000, 128000, Some("medium")),
    ("claude-fable-5", "claude-fable-5", Some(1000000), 64000, 128000, Some("high")),
    ("claude-fable-5-1", "claude-fable-5-1", Some(1000000), 64000, 128000, Some("high")),
    ("claude-mythos-5", "claude-mythos-5", Some(1000000), 64000, 128000, None),
    ("claude-mythos-5-1", "claude-mythos-5-1", Some(1000000), 64000, 128000, Some("high")),
];

/// `(tier, [input, output, cache_write_5m, cache_write_1h, cache_read, web_search], long prompt)`.
#[rustfmt::skip]
#[allow(clippy::type_complexity)]
const PRICING: [(&str, [f64; 6], Option<(u64, [f64; 5])>); 11] = [
    ("tier_2_10", [2.0, 10.0, 2.5, 4.0, 0.2, 0.01], None),
    ("tier_2_10_cache_read_0_10", [2.0, 10.0, 2.5, 4.0, 0.1, 0.01], None),
    ("tier_3_15", [3.0, 15.0, 3.75, 6.0, 0.3, 0.01], None),
    ("tier_5_25", [5.0, 25.0, 6.25, 10.0, 0.5, 0.01], None),
    ("tier_15_75", [15.0, 75.0, 18.75, 30.0, 1.5, 0.01], None),
    ("tier_10_50", [10.0, 50.0, 12.5, 20.0, 1.0, 0.01], None),
    ("tier_10_50_cache_read_0_25", [10.0, 50.0, 12.5, 20.0, 0.25, 0.01], None),
    ("tier_4_20_cache_read_0_20", [4.0, 20.0, 5.0, 8.0, 0.2, 0.01], None),
    ("haiku_35", [0.8, 4.0, 1.0, 1.6, 0.08, 0.01], None),
    ("haiku_45", [1.0, 5.0, 1.25, 2.0, 0.1, 0.01], None),
    ("haiku_55", [0.1, 0.5, 0.125, 0.2, 0.01, 0.01], Some((100000, [0.5, 2.5, 0.625, 1.0, 0.05]))),
];

#[test]
fn table_matches_the_baked_literal() {
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(models().len(), 22);
    assert_eq!(aliases().len(), 4);
    for (model, (id, first_party, window, default, upper, effort)) in models().iter().zip(MODELS) {
        assert_eq!(model.id, id);
        assert_eq!(model.provider_ids.first_party, first_party);
        assert_eq!(model.context.map(|c| c.window), window, "{id}");
        assert_eq!(
            (
                model.max_output_tokens.default,
                model.max_output_tokens.upper
            ),
            (default, upper),
            "{id}"
        );
        assert_eq!(model.default_effort, effort, "{id}");
    }
}

#[test]
fn pricing_tiers_match_and_every_model_names_one() {
    for (tier, prices, long) in PRICING {
        let got = pricing(tier).unwrap_or_else(|| panic!("{tier}"));
        let flat = [
            got.input,
            got.output,
            got.cache_write_5m,
            got.cache_write_1h,
            got.cache_read,
            got.web_search,
        ];
        assert_eq!(flat, prices, "{tier}");
        let got_long = got.long_prompt.map(|l| {
            (
                l.above_prompt_tokens,
                [
                    l.input,
                    l.output,
                    l.cache_write_5m,
                    l.cache_write_1h,
                    l.cache_read,
                ],
            )
        });
        assert_eq!(got_long, long, "{tier}");
    }
    assert!(pricing("nope").is_none());
    for model in models() {
        assert!(pricing(model.pricing).is_some(), "{}", model.id);
    }
}

#[test]
fn identifiers_do_not_collide_across_models() {
    for model in models() {
        assert_eq!(find(model.id).map(|m| m.id), Some(model.id));
        assert_eq!(
            find(model.provider_ids.first_party).map(|m| m.id),
            Some(model.id)
        );
        for provider in [
            CatalogProvider::Bedrock,
            CatalogProvider::Vertex,
            CatalogProvider::Foundry,
            CatalogProvider::AnthropicAws,
            CatalogProvider::AnthropicGoogleCloud,
            CatalogProvider::Mantle,
            CatalogProvider::Gateway,
        ] {
            if let Some(id) = model.id_for(provider) {
                assert_eq!(find(id).map(|m| m.id), Some(model.id), "{id}");
            }
        }
    }
}

#[test]
fn find_strips_the_one_million_suffix_case_insensitively() {
    assert_eq!(
        find("claude-opus-5-5[1m]").map(|m| m.id),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        find("claude-opus-5-5[1M]").map(|m| m.id),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        find("claude-haiku-4-5-20251001").map(|m| m.id),
        Some("claude-haiku-4-5")
    );
    assert_eq!(
        find("us.anthropic.claude-opus-5-5").map(|m| m.id),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        find("anthropic.claude-opus-5-5[1m]").map(|m| m.id),
        Some("claude-opus-5-5")
    );
    assert!(find("[1m]").is_none());
    assert!(find("").is_none());
    assert!(find("claude-opus-5-5[1m][1m]").is_none());
    assert!(find("claude-opus-9").is_none());
    assert!(find("é[1m").is_none());
}

#[test]
fn provider_ids_and_capabilities() {
    let opus = find("claude-opus-5-5").expect("opus 5.5");
    assert_eq!(
        opus.id_for(CatalogProvider::FirstParty),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        opus.id_for(CatalogProvider::Bedrock),
        Some("us.anthropic.claude-opus-5-5")
    );
    assert_eq!(
        opus.id_for(CatalogProvider::Mantle),
        Some("anthropic.claude-opus-5-5")
    );
    assert_eq!(opus.fallback_3p, Some("claude-opus-5"));
    assert_eq!(opus.advisor_rank, Some(7));
    assert!(opus.has("fast_mode") && opus.has("opus_5_5_prompt_bundle"));
    assert!(!opus.has("fable_5_mitigations"));
    assert_eq!(
        opus.effort_cost_index,
        Some(EffortCost {
            low: 0.48,
            medium: 0.66,
            high: 1.0,
            xhigh: 1.66,
            max: 3.44
        })
    );
    assert_eq!(
        opus.image_limits,
        Some(ImageLimits {
            max_width: 2576,
            max_height: 2576,
            max_image_tokens: 4784
        })
    );
    assert_eq!(
        opus.vertex_region_env_var,
        Some("VERTEX_REGION_CLAUDE_5_5_OPUS")
    );
    assert!(opus.eager_input_streaming.bedrock && opus.eager_input_streaming.vertex);
    let context = opus.context.expect("context");
    assert!(context.native_1m && context.supports_1m_beta && context.supports_1m_suffix);

    let legacy = find("claude-3-5-haiku").expect("haiku 3.5");
    assert_eq!(legacy.id_for(CatalogProvider::AnthropicGoogleCloud), None);
    assert_eq!(
        legacy.id_for(CatalogProvider::Vertex),
        Some("claude-3-5-haiku@20241022")
    );
    assert!(legacy.context.is_none() && legacy.image_limits.is_none());
    assert!(!legacy.eager_input_streaming.bedrock && legacy.eager_input_streaming.vertex);

    let sonnet = find("claude-sonnet-4-5").expect("sonnet 4.5");
    assert_eq!(
        sonnet.context,
        Some(Context {
            window: 200_000,
            maximum_window: Some(1_000_000),
            native_1m: false,
            supports_1m_beta: true,
            supports_1m_suffix: true,
            clamp_long_context_without_beta: true,
        })
    );
}

#[test]
fn aliases_resolve_per_provider() {
    use CatalogProvider::*;
    for (alias, provider, expected) in [
        ("opus", None, "claude-opus-5-5"),
        ("opus", Some(FirstParty), "claude-opus-5-5"),
        ("opus", Some(Bedrock), "claude-opus-5-5"),
        ("opus", Some(Foundry), "claude-opus-4-6"),
        ("opus", Some(Gateway), "claude-opus-4-7"),
        ("opus", Some(AnthropicGoogleCloud), "claude-opus-5-5"),
        ("sonnet", None, "claude-sonnet-5-5"),
        ("sonnet", Some(Bedrock), "claude-sonnet-4-5"),
        ("sonnet", Some(Mantle), "claude-sonnet-4-5"),
        ("sonnet", Some(AnthropicAws), "claude-sonnet-4-6"),
        ("sonnet", Some(Gateway), "claude-sonnet-4-6"),
        ("haiku", None, "claude-haiku-5-5"),
        ("haiku", Some(Vertex), "claude-haiku-4-5"),
        ("haiku", Some(AnthropicGoogleCloud), "claude-haiku-4-5"),
        ("fable", None, "claude-fable-5-1"),
        ("fable", Some(Gateway), "claude-fable-5"),
        ("fable", Some(Bedrock), "claude-fable-5-1"),
    ] {
        assert_eq!(
            resolve_alias(alias, provider),
            Some(expected),
            "{alias} {provider:?}"
        );
    }
    assert_eq!(resolve_alias("nope", None), None);
    for alias in aliases() {
        assert!(find(alias.default).is_some(), "{}", alias.name);
        for (_, id) in alias.per_provider {
            assert!(find(id).is_some(), "{id}");
        }
    }
}

#[test]
fn latest_per_family_and_best() {
    assert_eq!(BEST, "fable");
    assert_eq!(
        LATEST_PER_FAMILY,
        [
            ("fable", "claude-fable-5-1"),
            ("opus", "claude-opus-5-5"),
            ("sonnet", "claude-sonnet-5-5"),
            ("haiku", "claude-haiku-5-5"),
        ]
    );
    for (family, id) in LATEST_PER_FAMILY {
        assert_eq!(find(id).map(|m| m.family), Some(*family));
    }
}

#[test]
fn gateway_falls_back_to_the_first_party_id() {
    let named = find("claude-haiku-4-5").unwrap();
    assert_eq!(
        named.id_for(CatalogProvider::Gateway),
        Some("claude-haiku-4-5-20251001")
    );
    let unnamed = find("claude-haiku-5-5").unwrap();
    assert_eq!(unnamed.provider_ids.gateway, None);
    assert_eq!(
        unnamed.id_for(CatalogProvider::Gateway),
        Some(unnamed.provider_ids.first_party)
    );
    // Other providers keep their absent ids.
    let mythos = find("claude-mythos-5").unwrap();
    assert_eq!(mythos.id_for(CatalogProvider::Vertex), None);
    assert_eq!(
        mythos.id_for(CatalogProvider::Gateway),
        Some("claude-mythos-5")
    );
}

#[test]
fn eager_input_streaming_is_read_on_vertex_and_bedrock_default_endpoints() {
    let providers = [
        CatalogProvider::FirstParty,
        CatalogProvider::Bedrock,
        CatalogProvider::Vertex,
        CatalogProvider::Foundry,
        CatalogProvider::AnthropicAws,
        CatalogProvider::AnthropicGoogleCloud,
        CatalogProvider::Mantle,
        CatalogProvider::Gateway,
    ];
    for (id, bedrock, vertex) in [
        ("claude-opus-5-5", true, true),
        ("claude-3-5-haiku", false, true),
        ("claude-mythos-5", false, false),
    ] {
        let flag = find(id).unwrap().eager_input_streaming;
        for provider in providers {
            let expected = match provider {
                CatalogProvider::Bedrock => bedrock,
                CatalogProvider::Vertex => vertex,
                _ => false,
            };
            assert_eq!(flag.enabled(provider, true), expected, "{id} {provider:?}");
            assert!(
                !flag.enabled(provider, false),
                "{id} {provider:?} custom url"
            );
        }
    }
}
