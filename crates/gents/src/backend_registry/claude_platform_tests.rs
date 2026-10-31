//! Validation and credential handling of Claude platform backends.

use super::*;
use serde_json::{json, Value};

fn backend(kind: &str, endpoint: &str, auth: Value, platform: Value) -> InferenceBackend {
    let mut value = json!({
        "node_did": "did:key:backend-owner",
        "backend_id": "platform",
        "name": "Platform",
        "provider_kind": kind,
        "endpoint": endpoint,
        "auth": auth,
    });
    if !platform.is_null() {
        value["claude_platform"] = platform;
    }
    serde_json::from_value(value).unwrap()
}

fn key() -> Value {
    json!({"kind": "environment", "variable": "PLATFORM_TEST_KEY"})
}

fn aws() -> Value {
    json!({
        "kind": "aws_credentials",
        "access_key_id": "AKIDEXAMPLE",
        "secret_access_key": "secret-access-key-value",
        "session_token": "session-token-value",
    })
}

fn google() -> Value {
    json!({"kind": "google_credentials", "credentials_json": "{\"private_key\":\"google-secret\"}"})
}

/// Every platform kind with settings it accepts.
fn valid_backends() -> Vec<InferenceBackend> {
    vec![
        backend(
            "ClaudeGateway",
            "https://gateway.example",
            key(),
            Value::Null,
        ),
        backend(
            "ClaudeFoundry",
            "https://my-resource.services.ai.azure.com/anthropic/",
            key(),
            json!({"resource": "my-resource"}),
        ),
        backend(
            "ClaudeAnthropicAws",
            "https://aws-external-anthropic.us-west-2.api.aws",
            key(),
            json!({"region": "us-west-2", "workspace_id": "wrkspc_01"}),
        ),
        backend(
            "ClaudeMantle",
            "https://bedrock-mantle.us-west-2.api.aws/anthropic",
            key(),
            json!({"region": "us-west-2"}),
        ),
        backend(
            "ClaudeBedrock",
            "https://bedrock-runtime.us-west-2.amazonaws.com",
            aws(),
            json!({"region": "us-west-2"}),
        ),
        backend(
            "ClaudeVertex",
            "https://us-east5-aiplatform.googleapis.com/v1",
            google(),
            json!({"project": "proj-1"}),
        ),
        backend(
            "ClaudeGoogleCloud",
            "https://claude.googleapis.com/v1/projects/p/locations/global/workspaces/w/invoke",
            google(),
            json!({"project": "p", "workspace_id": "w"}),
        ),
    ]
}

#[test]
fn every_platform_kind_validates_with_its_settings_and_credential() {
    for backend in valid_backends() {
        backend
            .validate()
            .unwrap_or_else(|error| panic!("{}: {error:#}", backend.provider_kind));
    }
}

#[test]
fn a_platform_kind_without_its_settings_is_refused_with_the_missing_field() {
    for (kind, endpoint, auth, expected) in [
        ("ClaudeFoundry", "https://f.example", key(), "resource"),
        ("ClaudeAnthropicAws", "https://a.example", key(), "region"),
        ("ClaudeMantle", "https://m.example", key(), "region"),
        ("ClaudeBedrock", "https://b.example", aws(), "region"),
        ("ClaudeVertex", "https://v.example", google(), "project"),
        (
            "ClaudeGoogleCloud",
            "https://g.example",
            google(),
            "project",
        ),
    ] {
        let violations = backend(kind, endpoint, auth, Value::Null).validation_violations();
        assert_eq!(
            violations,
            vec![format!("{kind} needs the {expected} setting")],
            "{kind}"
        );
    }
}

#[test]
fn settings_outside_the_kind_are_refused_on_every_kind() {
    let violations = backend(
        "ClaudeGateway",
        "https://gateway.example",
        key(),
        json!({"region": "us-west-2"}),
    )
    .validation_violations();
    assert_eq!(
        violations,
        vec!["ClaudeGateway does not use the region setting"]
    );
    for kind in ["OpenAiCompatible", "OpenRouter", "AnthropicApiKey"] {
        let endpoint = if kind == "AnthropicApiKey" {
            crate::claude_subscription::ANTHROPIC_API_ENDPOINT
        } else {
            "https://example.test/v1"
        };
        let violations =
            backend(kind, endpoint, key(), json!({"region": "us-west-2"})).validation_violations();
        assert_eq!(
            violations,
            vec![format!("{kind} takes no claude_platform settings")],
            "{kind}"
        );
    }
}

#[test]
fn a_platform_credential_must_suit_the_platform() {
    let mismatch = "backend auth selection is incompatible with the provider adapter";
    let settings = json!({"region": "us-west-2", "workspace_id": "w"});
    for (kind, auth) in [
        ("ClaudeGateway", json!({"kind": "unauthenticated"})),
        ("ClaudeGateway", aws()),
        ("ClaudeGateway", google()),
        ("ClaudeFoundry", json!({"kind": "node_oauth"})),
        ("ClaudeMantle", google()),
        ("ClaudeAnthropicAws", google()),
        ("ClaudeVertex", aws()),
        ("ClaudeGoogleCloud", aws()),
        ("OpenAiCompatible", aws()),
        ("AnthropicApiKey", google()),
    ] {
        let settings = match kind {
            "ClaudeGateway" | "OpenAiCompatible" | "AnthropicApiKey" => Value::Null,
            "ClaudeFoundry" => json!({"resource": "my-resource"}),
            "ClaudeMantle" | "ClaudeBedrock" => json!({"region": "us-west-2"}),
            "ClaudeVertex" => json!({"project": "p"}),
            "ClaudeGoogleCloud" => json!({"project": "p", "workspace_id": "w"}),
            _ => settings.clone(),
        };
        let endpoint = if kind == "AnthropicApiKey" {
            crate::claude_subscription::ANTHROPIC_API_ENDPOINT
        } else {
            "https://example.test/v1"
        };
        let violations = backend(kind, endpoint, auth, settings).validation_violations();
        assert_eq!(violations, vec![mismatch], "{kind}");
    }
    for (kind, auth) in [
        ("ClaudeBedrock", aws()),
        ("ClaudeMantle", aws()),
        ("ClaudeAnthropicAws", aws()),
        ("ClaudeVertex", google()),
        ("ClaudeGoogleCloud", google()),
        ("ClaudeMantle", json!({"kind": "api_key", "key": "k"})),
        ("ClaudeVertex", key()),
    ] {
        assert!(
            auth.get("kind").is_some()
                && BackendAuth::suits_provider(
                    &serde_json::from_value(auth).unwrap(),
                    BackendProviderKind::parse_optional(Some(kind)).unwrap()
                ),
            "{kind}"
        );
    }
}

#[test]
fn a_platform_endpoint_must_be_an_http_url() {
    let violations =
        backend("ClaudeGateway", "gateway.example", key(), Value::Null).validation_violations();
    assert_eq!(violations, vec!["endpoint must be an http or https URL"]);
    backend("ClaudeGateway", "http://127.0.0.1:9", key(), Value::Null)
        .validate()
        .unwrap();
}

#[test]
fn settings_survive_the_document_round_trip_and_are_omitted_when_absent() {
    let foundry = backend(
        "ClaudeFoundry",
        "https://my-resource.services.ai.azure.com/anthropic/",
        key(),
        json!({"resource": "my-resource", "bearer_token": true}),
    );
    let stored = serde_json::to_value(&foundry).unwrap();
    assert_eq!(
        stored["claude_platform"],
        json!({"resource": "my-resource", "bearer_token": true})
    );
    let decoded = InferenceBackend::from_value(&stored).unwrap();
    assert_eq!(decoded.claude_platform, foundry.claude_platform);
    assert_eq!(
        decoded.backend_fields().claude_platform,
        foundry.claude_platform
    );
    let gateway = backend("ClaudeGateway", "https://g.example", key(), Value::Null);
    assert!(serde_json::to_value(&gateway)
        .unwrap()
        .get("claude_platform")
        .is_none());
    let unknown = serde_json::from_value::<InferenceBackend>(json!({
        "node_did": "d", "backend_id": "b", "name": "n", "provider_kind": "ClaudeGateway",
        "endpoint": "https://g.example", "auth": {"kind": "environment", "variable": "K"},
        "claude_platform": {"regon": "x"},
    }));
    assert!(unknown.is_err());
}

#[test]
fn cloud_credentials_decode_and_never_print_their_secrets() {
    let aws: BackendAuth = serde_json::from_value(aws()).unwrap();
    assert_eq!(serde_json::to_value(&aws).unwrap(), self::aws());
    for auth in [aws.clone(), serde_json::from_value(google()).unwrap()] {
        let printed = format!("{auth:?}");
        for secret in [
            "secret-access-key-value",
            "session-token-value",
            "google-secret",
        ] {
            assert!(!printed.contains(secret), "{printed}");
        }
        assert!(auth.is_stored_secret());
        assert_eq!(auth.probe_key().unwrap(), None);
        let error = auth.resolve_api_key().unwrap_err().to_string();
        assert!(!error.contains("secret"), "{error}");
    }
    let no_token: BackendAuth = serde_json::from_value(json!({
        "kind": "aws_credentials", "access_key_id": "AKID", "secret_access_key": "s"
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&no_token).unwrap(),
        json!({"kind": "aws_credentials", "access_key_id": "AKID", "secret_access_key": "s"})
    );
    let printed = format!(
        "{:?}",
        backend(
            "ClaudeBedrock",
            "https://b.example",
            self::aws(),
            json!({"region": "us-west-2"})
        )
    );
    assert!(!printed.contains("secret-access-key-value"), "{printed}");
    assert!(BackendAuth::ApiKey { key: "k".into() }.is_stored_secret());
    assert!(!BackendAuth::Environment {
        variable: "V".into()
    }
    .is_stored_secret());
}

#[test]
fn the_self_configuration_guard_keeps_cloud_credentials_operator_managed() {
    let doc = |auth: Value| match json!({ "auth": auth }) {
        Value::Object(map) => map,
        _ => unreachable!(),
    };
    for credentials in [aws(), google()] {
        let kept = crate::self_config::guard_backend_auth(
            &doc(credentials.clone()),
            &doc(credentials.clone()),
        );
        assert!(kept.is_ok(), "{kept:?}");
        let introduced = crate::self_config::guard_backend_auth(&doc(key()), &doc(credentials));
        let message = introduced.unwrap_err().to_string();
        assert!(message.contains("operator-managed"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }
    let mut changed = aws();
    changed["secret_access_key"] = "another-secret".into();
    assert!(crate::self_config::guard_backend_auth(&doc(aws()), &doc(changed)).is_err());
}

#[test]
fn the_connection_identity_covers_the_platform_settings() {
    let base = backend(
        "ClaudeAnthropicAws",
        "https://a.example",
        key(),
        json!({"region": "us-west-2", "workspace_id": "w1"}),
    );
    let moved = backend(
        "ClaudeAnthropicAws",
        "https://a.example",
        key(),
        json!({"region": "us-west-2", "workspace_id": "w2"}),
    );
    let fingerprint = |backend: &InferenceBackend| {
        crate::admission::backend_connection_fingerprint(&backend.backend_fields())
    };
    assert_eq!(fingerprint(&base), fingerprint(&base));
    assert_ne!(fingerprint(&base), fingerprint(&moved));
}
