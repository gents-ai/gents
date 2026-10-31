//! Loopback tests of the Claude platform routes: each backend kind sends the
//! request Claude Code 2.1.311 sends on that platform (path and query, header
//! names as the transport writes them, credential placement, body bytes) and
//! its streamed answer parses. Secrets are masked in every assertion.

use futures::StreamExt;
use gents_claude::wire;
use gents_loop::backend_provider::BackendProviderKind;
use gents_loop::claude_messages_body::plan_claude_request_for;
use gents_loop::claude_platform::ClaudePlatform;
use rapidhash::{HashSetExt, RapidHashSet};
use rig::streaming::RawStreamingChoice;

use super::lock_fixtures_for_test;
use super::route::{ClaudeRoute, ClaudeRouteError};
use super::send::stream_messages_routed;
use super::tests::echo_request;
use super::wire_tests::{reply, Server, Wire};
use crate::claude_subscription::{AccountIdentity, ApiKeyBearer};

const SECRET: &str = "platform-secret-credential";
const MODEL: &str = "claude-sonnet-5";

fn sse_body() -> &'static str {
    Box::leak(crate::claude_messages::sse_fixture_final_text("platform hello").into_boxed_str())
}

fn settings(build: impl FnOnce(&mut ClaudePlatform)) -> ClaudePlatform {
    let mut settings = ClaudePlatform::default();
    build(&mut settings);
    settings
}

fn route(
    kind: BackendProviderKind,
    server: &Server,
    path: &str,
    settings: Option<&ClaudePlatform>,
) -> ClaudeRoute {
    let endpoint = format!("http://127.0.0.1:{}{path}", server.port);
    ClaudeRoute::platform(kind, &endpoint, settings, SECRET).expect("platform route")
}

/// Starts the server first so its port can sit in the route's base URL.
async fn exchange(
    kind: BackendProviderKind,
    path: &str,
    settings: Option<ClaudePlatform>,
) -> (Server, Wire, ClaudeRoute) {
    let server = Server::start(vec![reply("200 OK", sse_body())]).await;
    let route = route(kind, &server, path, settings.as_ref());
    let account = AccountIdentity::of_platform("did:key:z6MkPlatform", "backend-1").context(false);
    let bearer = ApiKeyBearer::new(SECRET);
    let stream = stream_messages_routed(
        &route,
        MODEL,
        &echo_request(),
        &account,
        RapidHashSet::new(),
        &bearer,
        None,
    )
    .await
    .expect("200");
    futures::pin_mut!(stream);
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        match item.expect("event") {
            RawStreamingChoice::Message(chunk) => text.push_str(&chunk),
            RawStreamingChoice::FinalResponse(_) => break,
            _ => {}
        }
    }
    assert_eq!(text, "platform hello");
    let wire = server.requests().remove(0);
    (server, wire, route)
}

fn masked(wire: &Wire, name: &str) -> Option<String> {
    wire.header(name)
        .map(|value| value.replace(SECRET, "<secret>"))
}

/// The header names in the order the transport writes them: sorted by code point,
/// then Bun's appended defaults.
fn expected_names(extra: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = [
        "Accept",
        "Content-Type",
        "User-Agent",
        "X-Claude-Code-Session-Id",
        "X-Stainless-Arch",
        "X-Stainless-Lang",
        "X-Stainless-OS",
        "X-Stainless-Package-Version",
        "X-Stainless-Retry-Count",
        "X-Stainless-Runtime",
        "X-Stainless-Runtime-Version",
        "X-Stainless-Timeout",
        "anthropic-beta",
        "anthropic-dangerous-direct-browser-access",
        "anthropic-version",
        "x-app",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    // The HTTP layer lowercases every name; only WebKit's common names keep their spelling.
    .map(|name| match name {
        "Accept" | "Authorization" | "Content-Type" | "User-Agent" => name.to_owned(),
        other => other.to_ascii_lowercase(),
    })
    .collect();
    names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    names.extend(
        ["Connection", "Host", "Accept-Encoding", "Content-Length"]
            .into_iter()
            .map(str::to_owned),
    );
    names
}

fn names(wire: &Wire) -> Vec<String> {
    wire.headers.iter().map(|(name, _)| name.clone()).collect()
}

/// The body bytes the planner types for `route`'s platform.
fn expected_body(route: &ClaudeRoute) -> Vec<u8> {
    let account = AccountIdentity::of_platform("did:key:z6MkPlatform", "backend-1").context(false);
    let plan = plan_claude_request_for(MODEL, &echo_request(), &account, route.target).unwrap();
    wire::to_bytes(&plan.body).unwrap()
}

fn assert_platform_request(wire: &Wire, route: &ClaudeRoute, path: &str) {
    assert_eq!(
        wire.request_line,
        format!("POST {path}/v1/messages?beta=true HTTP/1.1")
    );
    assert_eq!(wire.body, expected_body(route));
    assert_eq!(
        wire.header("content-length"),
        Some(wire.body.len().to_string().as_str())
    );
    assert!(wire.header("content-encoding").is_none());
    let betas = wire.header("anthropic-beta").expect("beta header");
    assert!(!betas.contains("oauth-2025-04-20"), "{betas}");
    assert!(betas.contains("claude-code-20250219"), "{betas}");
    assert_eq!(wire.header("anthropic-version"), Some("2023-06-01"));
    for hint in [
        "x-claude-code-request-class",
        "x-claude-code-agent-type",
        "x-claude-code-prompt-id",
    ] {
        assert!(wire.header(hint).is_none(), "{hint}");
    }
    let body = String::from_utf8_lossy(&wire.body);
    assert!(body.contains(r#""account_uuid\":\"\""#), "{body}");
    assert!(!body.contains("cch=00000"), "{body}");
}

#[tokio::test]
async fn a_gateway_sends_a_bearer_jwt_without_a_client_request_id() {
    let _guard = lock_fixtures_for_test().await;
    let (_server, wire, route) = exchange(BackendProviderKind::ClaudeGateway, "", None).await;
    assert_platform_request(&wire, &route, "");
    assert_eq!(
        masked(&wire, "authorization").as_deref(),
        Some("Bearer <secret>")
    );
    assert_eq!(names(&wire), expected_names(&["Authorization"]));
    let body = String::from_utf8_lossy(&wire.body);
    // The token is not a gateway JWT, so the attribution marks an unvouched gateway.
    assert!(body.contains("cc_gateway=env;"), "{body}");
}

#[tokio::test]
async fn foundry_sends_an_api_key_under_its_anthropic_base() {
    let _guard = lock_fixtures_for_test().await;
    let foundry = settings(|s| s.resource = Some("my-resource".into()));
    let (_server, wire, route) = exchange(
        BackendProviderKind::ClaudeFoundry,
        "/anthropic/",
        Some(foundry),
    )
    .await;
    assert_platform_request(&wire, &route, "/anthropic");
    assert_eq!(masked(&wire, "x-api-key").as_deref(), Some("<secret>"));
    assert!(wire.header("authorization").is_none());
    assert_eq!(names(&wire), expected_names(&["x-api-key"]));
    let betas = wire.header("anthropic-beta").unwrap();
    assert!(betas.contains("web-search"), "{betas}");
}

#[tokio::test]
async fn foundry_sends_a_static_token_as_a_bearer_when_the_backend_says_so() {
    let _guard = lock_fixtures_for_test().await;
    let foundry = settings(|s| {
        s.resource = Some("my-resource".into());
        s.bearer_token = Some(true);
    });
    let (_server, wire, route) = exchange(
        BackendProviderKind::ClaudeFoundry,
        "/anthropic/",
        Some(foundry),
    )
    .await;
    assert_platform_request(&wire, &route, "/anthropic");
    assert_eq!(
        masked(&wire, "authorization").as_deref(),
        Some("Bearer <secret>")
    );
    assert!(wire.header("x-api-key").is_none());
    assert_eq!(names(&wire), expected_names(&["Authorization"]));
}

#[tokio::test]
async fn claude_platform_on_aws_sends_the_key_and_the_workspace() {
    let _guard = lock_fixtures_for_test().await;
    let aws = settings(|s| {
        s.region = Some("us-west-2".into());
        s.workspace_id = Some("wrkspc_01".into());
    });
    let (_server, wire, route) =
        exchange(BackendProviderKind::ClaudeAnthropicAws, "", Some(aws)).await;
    assert_platform_request(&wire, &route, "");
    assert_eq!(masked(&wire, "x-api-key").as_deref(), Some("<secret>"));
    assert_eq!(wire.header("anthropic-workspace-id"), Some("wrkspc_01"));
    // A base URL other than the default sends no client request id.
    assert_eq!(
        names(&wire),
        expected_names(&["X-Api-Key", "anthropic-workspace-id"])
    );
    let betas = wire.header("anthropic-beta").unwrap();
    assert!(betas.contains("prompt-caching-scope"), "{betas}");
}

#[tokio::test]
async fn claude_platform_on_aws_sends_a_client_request_id_only_on_its_default_url() {
    let _guard = lock_fixtures_for_test().await;
    let aws = settings(|s| {
        s.region = Some("us-west-2".into());
        s.workspace_id = Some("wrkspc_01".into());
    });
    let default =
        ClaudePlatform::default_endpoint(BackendProviderKind::ClaudeAnthropicAws, Some(&aws))
            .unwrap()
            .unwrap();
    let on_default = ClaudeRoute::platform(
        BackendProviderKind::ClaudeAnthropicAws,
        &default,
        Some(&aws),
        SECRET,
    )
    .unwrap();
    assert_eq!(
        on_default.uri,
        "https://aws-external-anthropic.us-west-2.api.aws/v1/messages?beta=true"
    );
    assert!(on_default.client_request_id_header);
    // The same route aimed at the loopback server shows the header on the wire.
    let server = Server::start(vec![reply("200 OK", sse_body())]).await;
    let local = ClaudeRoute {
        uri: format!("http://127.0.0.1:{}/v1/messages?beta=true", server.port),
        ..on_default
    };
    let account = AccountIdentity::of_platform("did:key:z6MkPlatform", "backend-1").context(false);
    let stream = stream_messages_routed(
        &local,
        MODEL,
        &echo_request(),
        &account,
        RapidHashSet::new(),
        &ApiKeyBearer::new(SECRET),
        None,
    )
    .await
    .expect("200");
    drop(stream);
    let wire = server.requests().remove(0);
    assert_eq!(
        names(&wire),
        expected_names(&["X-Api-Key", "anthropic-workspace-id", "x-client-request-id"])
    );
    let other = ClaudeRoute::platform(
        BackendProviderKind::ClaudeAnthropicAws,
        "https://proxy.example",
        Some(&aws),
        SECRET,
    )
    .unwrap();
    assert!(!other.client_request_id_header);
}

#[tokio::test]
async fn mantle_sends_the_bearer_token_under_its_anthropic_base() {
    let _guard = lock_fixtures_for_test().await;
    let mantle = settings(|s| s.region = Some("us-east-1".into()));
    let (_server, wire, route) = exchange(
        BackendProviderKind::ClaudeMantle,
        "/anthropic",
        Some(mantle),
    )
    .await;
    assert_platform_request(&wire, &route, "/anthropic");
    assert_eq!(
        masked(&wire, "authorization").as_deref(),
        Some("Bearer <secret>")
    );
    assert_eq!(names(&wire), expected_names(&["Authorization"]));
    let body = String::from_utf8_lossy(&wire.body);
    assert!(
        body.contains(r#""model":"anthropic.claude-sonnet-5""#),
        "{body}"
    );
}

#[tokio::test]
async fn a_rejected_credential_is_reported_without_a_second_send() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![
        reply(
            "401 Unauthorized",
            r#"{"type":"error","error":{"type":"authentication_error","message":"bad token"}}"#,
        ),
        reply("200 OK", sse_body()),
    ])
    .await;
    let route = route(BackendProviderKind::ClaudeGateway, &server, "", None);
    let account = AccountIdentity::of_platform("did:key:z6MkPlatform", "backend-1").context(false);
    let error = stream_messages_routed(
        &route,
        MODEL,
        &echo_request(),
        &account,
        RapidHashSet::new(),
        &ApiKeyBearer::new(SECRET),
        None,
    )
    .await
    .err()
    .expect("401");
    assert!(error.to_string().contains("401"), "{error}");
    assert!(!error.to_string().contains(SECRET), "{error}");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn the_platforms_without_a_send_path_say_so() {
    for (kind, settings) in [
        (
            BackendProviderKind::ClaudeBedrock,
            self::settings(|s| s.region = Some("us-east-1".into())),
        ),
        (
            BackendProviderKind::ClaudeVertex,
            self::settings(|s| s.project = Some("proj".into())),
        ),
        (
            BackendProviderKind::ClaudeGoogleCloud,
            self::settings(|s| {
                s.project = Some("proj".into());
                s.workspace_id = Some("ws".into());
            }),
        ),
    ] {
        let error = ClaudeRoute::platform(kind, "https://example.test", Some(&settings), SECRET)
            .expect_err("not built");
        assert!(
            matches!(error, ClaudeRouteError::Unavailable { .. }),
            "{kind}"
        );
    }
    assert!(matches!(
        ClaudeRoute::platform(
            BackendProviderKind::ClaudeMantle,
            "https://example.test",
            None,
            SECRET
        ),
        Err(ClaudeRouteError::Settings(_))
    ));
    assert!(matches!(
        ClaudeRoute::platform(
            BackendProviderKind::AnthropicApiKey,
            "https://example.test",
            None,
            SECRET
        ),
        Err(ClaudeRouteError::NotAPlatform { .. })
    ));
}

#[test]
fn a_platform_account_has_no_account_uuid_and_a_device_id_per_node_and_backend() {
    let a = AccountIdentity::of_platform("did:key:z6MkA", "backend-1").context(false);
    let again = AccountIdentity::of_platform("did:key:z6MkA", "backend-1").context(false);
    let other_backend = AccountIdentity::of_platform("did:key:z6MkA", "backend-2").context(false);
    let other_node = AccountIdentity::of_platform("did:key:z6MkB", "backend-1").context(false);
    assert_eq!(a, again);
    assert_ne!(a.device_id, other_backend.device_id);
    assert_ne!(a.device_id, other_node.device_id);
    assert_eq!(a.device_id.len(), 64);
    assert!(a.account_uuid.is_empty());
    assert!(!a.subscriber && !a.api_key);
}
