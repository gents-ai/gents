use super::*;
use BackendProviderKind as Kind;

fn settings(build: impl FnOnce(&mut ClaudePlatform)) -> ClaudePlatform {
    let mut settings = ClaudePlatform::default();
    build(&mut settings);
    settings
}

fn region(value: &str) -> ClaudePlatform {
    settings(|s| s.region = Some(value.into()))
}

#[test]
fn each_kind_needs_its_required_settings() {
    for (kind, expected) in [
        (Kind::ClaudeBedrock, "region"),
        (Kind::ClaudeMantle, "region"),
        (Kind::ClaudeAnthropicAws, "region"),
        (Kind::ClaudeFoundry, "resource"),
        (Kind::ClaudeVertex, "project"),
        (Kind::ClaudeGoogleCloud, "project"),
    ] {
        assert_eq!(
            ClaudePlatform::validate(kind, None),
            Err(ClaudePlatformError::Missing {
                kind,
                field: expected
            }),
            "{kind}"
        );
    }
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeAnthropicAws, Some(&region("us-west-2")),),
        Err(ClaudePlatformError::Missing {
            kind: Kind::ClaudeAnthropicAws,
            field: "workspace_id"
        })
    );
    assert_eq!(ClaudePlatform::validate(Kind::ClaudeGateway, None), Ok(()));
}

#[test]
fn a_setting_outside_the_kind_is_refused() {
    let project = settings(|s| {
        s.region = Some("us-west-2".into());
        s.project = Some("proj".into());
    });
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeBedrock, Some(&project)),
        Err(ClaudePlatformError::Refused {
            kind: Kind::ClaudeBedrock,
            field: "project"
        })
    );
    let bearer = settings(|s| {
        s.region = Some("us-west-2".into());
        s.bearer_token = Some(false);
    });
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeMantle, Some(&bearer)),
        Err(ClaudePlatformError::Refused {
            kind: Kind::ClaudeMantle,
            field: "bearer_token"
        })
    );
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeGateway, Some(&region("us-west-2"))),
        Err(ClaudePlatformError::Refused {
            kind: Kind::ClaudeGateway,
            field: "region"
        })
    );
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeGateway, Some(&ClaudePlatform::default())),
        Ok(())
    );
}

#[test]
fn a_kind_that_is_not_a_platform_takes_no_settings() {
    for kind in [
        Kind::OpenAiCompatible,
        Kind::OpenRouter,
        Kind::ChatGptCodex,
        Kind::XaiGrokOAuth,
        Kind::ClaudeCliSubscription,
        Kind::AnthropicApiKey,
    ] {
        assert_eq!(ClaudePlatform::validate(kind, None), Ok(()));
        assert_eq!(
            ClaudePlatform::validate(kind, Some(&ClaudePlatform::default())),
            Err(ClaudePlatformError::NotAPlatform { kind })
        );
        assert!(!kind.is_claude_platform());
    }
}

#[test]
fn optional_settings_are_accepted_where_the_shape_lists_them() {
    let vertex = settings(|s| {
        s.project = Some("proj".into());
        s.location = Some("europe-west4".into());
    });
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeVertex, Some(&vertex)),
        Ok(())
    );
    let foundry = settings(|s| {
        s.resource = Some("my-resource".into());
        s.bearer_token = Some(true);
    });
    assert_eq!(
        ClaudePlatform::validate(Kind::ClaudeFoundry, Some(&foundry)),
        Ok(())
    );
}

#[test]
fn spellings_follow_the_checks_claude_code_makes() {
    for bad in [
        "",
        "US-WEST-2",
        "u-west-2",
        "us-west-2-a-b-c-d",
        "us--west",
        "us west",
    ] {
        assert!(
            matches!(
                ClaudePlatform::validate(Kind::ClaudeBedrock, Some(&region(bad))),
                Err(ClaudePlatformError::Invalid {
                    field: "region",
                    ..
                })
            ),
            "{bad:?}"
        );
    }
    for good in [
        "us-east-1",
        "eu-central-1",
        "ap-southeast-2",
        "us-gov-west-1",
    ] {
        assert_eq!(
            ClaudePlatform::validate(Kind::ClaudeBedrock, Some(&region(good))),
            Ok(()),
            "{good}"
        );
    }
    let google = |project: &str| {
        settings(|s| {
            s.project = Some(project.into());
            s.workspace_id = Some("ws-1".into());
        })
    };
    for bad in ["", "has space", "dot.ted", "slash/path"] {
        assert!(
            matches!(
                ClaudePlatform::validate(Kind::ClaudeGoogleCloud, Some(&google(bad))),
                Err(ClaudePlatformError::Invalid {
                    field: "project",
                    ..
                })
            ),
            "{bad:?}"
        );
    }
    let resource = |value: &str| settings(|s| s.resource = Some(value.into()));
    for bad in [
        "",
        "a",
        "-ab",
        "ab-",
        "my.resource",
        "https://x.services.ai.azure.com",
    ] {
        assert!(
            matches!(
                ClaudePlatform::validate(Kind::ClaudeFoundry, Some(&resource(bad))),
                Err(ClaudePlatformError::Invalid {
                    field: "resource",
                    ..
                })
            ),
            "{bad:?}"
        );
    }
    for good in ["ab", "My-Resource", &"a".repeat(64)] {
        assert_eq!(
            ClaudePlatform::validate(Kind::ClaudeFoundry, Some(&resource(good))),
            Ok(()),
            "{good}"
        );
    }
    assert!(matches!(
        ClaudePlatform::validate(Kind::ClaudeFoundry, Some(&resource(&"a".repeat(65)))),
        Err(ClaudePlatformError::Invalid {
            field: "resource",
            ..
        })
    ));
}

#[test]
fn default_endpoints_derive_from_the_settings() {
    let aws = settings(|s| {
        s.region = Some("us-west-2".into());
        s.workspace_id = Some("wrkspc_01".into());
    });
    let google = settings(|s| {
        s.project = Some("proj-1".into());
        s.workspace_id = Some("ws-1".into());
    });
    let vertex = |location: Option<&str>| {
        settings(|s| {
            s.project = Some("proj-1".into());
            s.location = location.map(str::to_owned);
        })
    };
    let foundry = settings(|s| s.resource = Some("my-resource".into()));
    let derive = |kind, settings: &ClaudePlatform| {
        ClaudePlatform::default_endpoint(kind, Some(settings)).unwrap()
    };
    assert_eq!(
        derive(Kind::ClaudeBedrock, &aws_region_only()).as_deref(),
        Some("https://bedrock-runtime.us-west-2.amazonaws.com")
    );
    assert_eq!(
        derive(Kind::ClaudeMantle, &aws_region_only()).as_deref(),
        Some("https://bedrock-mantle.us-west-2.api.aws/anthropic")
    );
    assert_eq!(
        derive(Kind::ClaudeAnthropicAws, &aws).as_deref(),
        Some("https://aws-external-anthropic.us-west-2.api.aws")
    );
    assert_eq!(
        derive(Kind::ClaudeFoundry, &foundry).as_deref(),
        Some("https://my-resource.services.ai.azure.com/anthropic/")
    );
    for (location, expected) in [
        (None, "https://us-east5-aiplatform.googleapis.com/v1"),
        (Some("global"), "https://aiplatform.googleapis.com/v1"),
        (Some("us"), "https://aiplatform.us.rep.googleapis.com/v1"),
        (Some("eu"), "https://aiplatform.eu.rep.googleapis.com/v1"),
        (
            Some("europe-west4"),
            "https://europe-west4-aiplatform.googleapis.com/v1",
        ),
    ] {
        assert_eq!(
            derive(Kind::ClaudeVertex, &vertex(location)).as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        derive(Kind::ClaudeGoogleCloud, &google).as_deref(),
        Some(
            "https://claude.googleapis.com/v1/projects/proj-1/locations/global/workspaces/ws-1/invoke"
        )
    );
    assert_eq!(
        ClaudePlatform::default_endpoint(Kind::ClaudeGateway, None),
        Ok(None)
    );
    assert_eq!(
        ClaudePlatform::default_endpoint(Kind::AnthropicApiKey, None),
        Ok(None)
    );
    assert!(ClaudePlatform::default_endpoint(Kind::ClaudeBedrock, None).is_err());
}

fn aws_region_only() -> ClaudePlatform {
    region("us-west-2")
}

#[test]
fn the_default_endpoint_test_ignores_a_trailing_slash() {
    let foundry = settings(|s| s.resource = Some("my-resource".into()));
    for endpoint in [
        "https://my-resource.services.ai.azure.com/anthropic/",
        "https://my-resource.services.ai.azure.com/anthropic",
    ] {
        assert!(ClaudePlatform::is_default_endpoint(
            Kind::ClaudeFoundry,
            Some(&foundry),
            endpoint
        ));
    }
    assert!(!ClaudePlatform::is_default_endpoint(
        Kind::ClaudeFoundry,
        Some(&foundry),
        "http://127.0.0.1:1"
    ));
    assert!(!ClaudePlatform::is_default_endpoint(
        Kind::ClaudeGateway,
        None,
        "https://gateway.example"
    ));
}

#[test]
fn messages_uri_joins_with_one_slash() {
    assert_eq!(
        messages_uri("https://x.services.ai.azure.com/anthropic/"),
        "https://x.services.ai.azure.com/anthropic/v1/messages?beta=true"
    );
    assert_eq!(
        messages_uri("https://gateway.example"),
        "https://gateway.example/v1/messages?beta=true"
    );
}

#[test]
fn settings_round_trip_and_refuse_unknown_fields() {
    let parsed: ClaudePlatform =
        serde_json::from_str(r#"{"region":"us-west-2","workspace_id":"w"}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&parsed).unwrap(),
        r#"{"region":"us-west-2","workspace_id":"w"}"#
    );
    assert!(serde_json::from_str::<ClaudePlatform>(r#"{"regon":"x"}"#).is_err());
}

#[test]
fn the_provider_kinds_map_to_claude_codes_providers() {
    use gents_claude::betas::Provider;
    for (kind, provider) in [
        (Kind::ClaudeCliSubscription, Provider::FirstParty),
        (Kind::AnthropicApiKey, Provider::FirstParty),
        (Kind::ClaudeBedrock, Provider::Bedrock),
        (Kind::ClaudeVertex, Provider::Vertex),
        (Kind::ClaudeFoundry, Provider::Foundry),
        (Kind::ClaudeAnthropicAws, Provider::AnthropicAws),
        (Kind::ClaudeMantle, Provider::Mantle),
        (Kind::ClaudeGoogleCloud, Provider::AnthropicGoogleCloud),
        (Kind::ClaudeGateway, Provider::Gateway),
    ] {
        assert_eq!(kind.claude_provider(), Some(provider));
        assert!(kind.uses_messages_wire());
    }
    for kind in [
        Kind::OpenAiCompatible,
        Kind::OpenRouter,
        Kind::ChatGptCodex,
        Kind::XaiGrokOAuth,
    ] {
        assert_eq!(kind.claude_provider(), None);
        assert!(!kind.uses_messages_wire());
    }
    for kind in Kind::ALL {
        assert_eq!(Kind::parse_optional(Some(kind.as_str())).unwrap(), kind);
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{}\"", kind.as_str())
        );
    }
}
