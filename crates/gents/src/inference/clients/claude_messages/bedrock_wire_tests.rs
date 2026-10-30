//! Loopback tests of the Amazon Bedrock route and of the AWS-signed Mantle and
//! Claude Platform on AWS routes: the exact request (line, signed header set,
//! the signature recomputed from the wire's own inputs, body bytes), the event
//! stream parsed into the events the SSE path produces, in-stream exceptions,
//! the region-prefix rewrite, inference-profile discovery and its cache, and
//! application inference profile ARNs. No secret appears in an assertion.

use std::sync::Arc;

use base64::Engine;
use futures::StreamExt;
use gents_claude::eventstream::{encode, Frame, HeaderValue};
use gents_claude::sigv4::{Credentials, Request as SigRequest, Scope, Signer};
use gents_claude::wire;
use gents_loop::backend_provider::BackendProviderKind;
use gents_loop::claude_messages_body::plan_claude_request_for;
use gents_loop::claude_platform::ClaudePlatform;
use rapidhash::{HashSetExt, RapidHashSet};
use rig::completion::CompletionError;
use rig::streaming::RawStreamingChoice;

use super::bedrock::clear_profile_cache_for_test;
use super::lock_fixtures_for_test;
use super::route::ClaudeRoute;
use super::send::stream_messages_routed;
use super::tests::echo_request;
use super::wire_tests::{raw_reply, reply, Reply, Server, Wire};
use crate::claude_subscription::{AccountIdentity, ApiKeyBearer, ClaudeStreamResponse};

const ACCESS_KEY: &str = "AKIDEXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const SESSION_TOKEN: &str = "session-token-value";
const BEARER: &str = "bedrock-bearer-secret";
const MODEL: &str = "claude-sonnet-5";
const EVENT_STREAM: &[(&str, &str)] = &[("content-type", "application/vnd.amazon.eventstream")];

type Events = Vec<RawStreamingChoice<ClaudeStreamResponse>>;

fn settings(region: &str) -> ClaudePlatform {
    ClaudePlatform {
        region: Some(region.into()),
        ..ClaudePlatform::default()
    }
}

fn signer() -> Arc<Signer> {
    Arc::new(Signer::new(
        Credentials::new(ACCESS_KEY, SECRET_KEY, Some(SESSION_TOKEN)).unwrap(),
    ))
}

fn leak(bytes: Vec<u8>) -> &'static [u8] {
    Box::leak(bytes.into_boxed_slice())
}

fn chunk_frame(json: &str) -> Vec<u8> {
    let payload = format!(
        r#"{{"bytes":"{}"}}"#,
        base64::engine::general_purpose::STANDARD.encode(json)
    );
    encode(&Frame {
        headers: vec![
            (":event-type".into(), HeaderValue::String("chunk".into())),
            (
                ":content-type".into(),
                HeaderValue::String("application/json".into()),
            ),
            (":message-type".into(), HeaderValue::String("event".into())),
        ],
        payload: payload.into_bytes(),
    })
}

fn exception_frame(name: &str, message: &str) -> Vec<u8> {
    encode(&Frame {
        headers: vec![
            (":exception-type".into(), HeaderValue::String(name.into())),
            (
                ":content-type".into(),
                HeaderValue::String("application/json".into()),
            ),
            (
                ":message-type".into(),
                HeaderValue::String("exception".into()),
            ),
        ],
        payload: format!(r#"{{"message":"{message}"}}"#).into_bytes(),
    })
}

/// The `data:` JSON of each event of an SSE text.
fn sse_payloads(sse: &str) -> Vec<String> {
    sse.split("\n\n")
        .filter_map(|block| block.lines().find_map(|line| line.strip_prefix("data: ")))
        .map(str::to_owned)
        .collect()
}

fn event_stream_of(sse: &str) -> Vec<u8> {
    sse_payloads(sse)
        .iter()
        .flat_map(|json| chunk_frame(json))
        .collect()
}

fn answer(sse: &str) -> Reply {
    raw_reply(EVENT_STREAM, leak(event_stream_of(sse)))
}

fn profiles(ids: &[&str], next: Option<&str>) -> Reply {
    let summaries: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"inferenceProfileId":"{id}"}}"#))
        .collect();
    let next = next.map_or(String::new(), |token| format!(r#","nextToken":"{token}""#));
    reply(
        "200 OK",
        Box::leak(
            format!(
                r#"{{"inferenceProfileSummaries":[{}]{next}}}"#,
                summaries.join(",")
            )
            .into_boxed_str(),
        ),
    )
}

fn account() -> crate::claude_subscription::AccountIdentity {
    AccountIdentity::of_platform("did:key:z6MkBedrock", "backend-bedrock")
}

fn bedrock_route(server: &Server, region: &str, backend: &str, signed: bool) -> ClaudeRoute {
    let endpoint = format!("http://127.0.0.1:{}", server.port);
    let settings = settings(region);
    let route = if signed {
        ClaudeRoute::platform_signed(
            BackendProviderKind::ClaudeBedrock,
            &endpoint,
            Some(&settings),
            signer(),
        )
    } else {
        ClaudeRoute::platform(
            BackendProviderKind::ClaudeBedrock,
            &endpoint,
            Some(&settings),
            BEARER,
        )
    };
    route.expect("bedrock route").with_backend_id(backend)
}

/// Sends the echo request and collects the events and the first failure.
async fn run(route: &ClaudeRoute, model: &str) -> (Events, Option<String>) {
    let context = account().context(false);
    let stream = match stream_messages_routed(
        route,
        model,
        &echo_request(),
        &context,
        RapidHashSet::new(),
        &ApiKeyBearer::new(BEARER),
        None,
    )
    .await
    {
        Ok(stream) => stream,
        Err(error) => return (Vec::new(), Some(error_text(&error))),
    };
    futures::pin_mut!(stream);
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(error) => return (events, Some(error_text(&error))),
        }
    }
    (events, None)
}

fn error_text(error: &CompletionError) -> String {
    error.to_string()
}

fn text_of(events: &Events) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            RawStreamingChoice::Message(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn request_target(wire: &Wire) -> &str {
    wire.request_line.split(' ').nth(1).expect("request target")
}

fn unix_of(amz_date: &str) -> u64 {
    let parsed = chrono::NaiveDateTime::parse_from_str(amz_date, "%Y%m%dT%H%M%SZ").unwrap();
    u64::try_from(parsed.and_utc().timestamp()).unwrap()
}

/// Signs the request the wire shows with the wire's own inputs and returns the
/// authorization the signer produces: the headers the client set, the target, the body.
fn recomputed_authorization(wire: &Wire, scope: &Scope<'_>, session_token: Option<&str>) -> String {
    let target = request_target(wire);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let pairs: Vec<(&str, &str)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .collect();
    // What the transport and the signature add around the client's headers.
    let skipped = [
        "authorization",
        "x-amz-date",
        "x-amz-security-token",
        "x-amz-content-sha256",
        "x-client-request-id",
        "host",
        "content-length",
        "connection",
        "accept-encoding",
    ];
    let headers: Vec<(&str, &str)> = wire
        .headers
        .iter()
        .filter(|(name, _)| !skipped.contains(&name.to_ascii_lowercase().as_str()))
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let signer = Signer::new(Credentials::new(ACCESS_KEY, SECRET_KEY, session_token).unwrap());
    let added = signer
        .sign(
            &SigRequest {
                method: "POST",
                path,
                query: &pairs,
                host: wire.header("host").expect("host"),
                headers: &headers,
                body: &wire.body,
            },
            scope,
            unix_of(wire.header("x-amz-date").expect("x-amz-date")),
        )
        .unwrap();
    added
        .into_iter()
        .find(|(name, _)| *name == "authorization")
        .map(|(_, value)| value.value)
        .expect("authorization")
}

fn signed_headers(authorization: &str) -> &str {
    authorization
        .split("SignedHeaders=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect("signed headers")
}

const CLIENT_SIGNED: &str = "accept;anthropic-beta;anthropic-dangerous-direct-browser-access;anthropic-version;content-type;host;x-amz-content-sha256;x-amz-date";
const CLIENT_SIGNED_TAIL: &str = "x-app;x-claude-code-session-id;x-stainless-arch;x-stainless-lang;x-stainless-os;x-stainless-package-version;x-stainless-retry-count;x-stainless-runtime;x-stainless-runtime-version;x-stainless-timeout";

fn expected_signed(session_token: bool) -> String {
    let token = if session_token {
        ";x-amz-security-token"
    } else {
        ""
    };
    format!("{CLIENT_SIGNED}{token};{CLIENT_SIGNED_TAIL}")
}

fn planned_body(route: &ClaudeRoute, model: &str) -> Vec<u8> {
    let plan = plan_claude_request_for(
        model,
        &echo_request(),
        &account().context(false),
        route.target,
    )
    .unwrap();
    wire::to_bytes(&plan.body).unwrap()
}

fn invoke_target(model: &str) -> String {
    gents_claude::bedrock::invoke_path(model).unwrap()
}

#[tokio::test]
async fn a_signed_bedrock_request_is_the_sdk_request_for_a_listed_profile() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("bedrock hello");
    let server = Server::start(vec![
        profiles(
            &[
                "us.anthropic.claude-sonnet-5",
                "eu.anthropic.claude-sonnet-5",
                "us.amazon.nova-pro-v1:0",
            ],
            None,
        ),
        answer(&sse),
    ])
    .await;
    let route = bedrock_route(&server, "eu-west-1", "list-1", true);
    let (events, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None);
    assert_eq!(text_of(&events), "bedrock hello");

    let requests = server.requests();
    let control = &requests[0];
    assert_eq!(
        control.request_line,
        "GET /inference-profiles?type=SYSTEM_DEFINED HTTP/1.1"
    );
    assert!(control
        .header("authorization")
        .unwrap()
        .contains("/eu-west-1/bedrock/aws4_request"));
    assert_eq!(control.header("x-amz-security-token"), Some(SESSION_TOKEN));

    // The region's prefix picks the eu profile of the account.
    let wire = &requests[1];
    assert_eq!(
        wire.request_line,
        format!(
            "POST {} HTTP/1.1",
            invoke_target("eu.anthropic.claude-sonnet-5")
        )
    );
    let authorization = wire.header("authorization").unwrap();
    assert!(authorization.starts_with(&format!("AWS4-HMAC-SHA256 Credential={ACCESS_KEY}/")));
    assert!(authorization.contains("/eu-west-1/bedrock/aws4_request, SignedHeaders="));
    assert_eq!(signed_headers(authorization), expected_signed(true));
    let scope = Scope {
        region: "eu-west-1",
        service: "bedrock",
    };
    assert_eq!(
        authorization,
        recomputed_authorization(wire, &scope, Some(SESSION_TOKEN))
    );
    assert_eq!(wire.header("x-amz-security-token"), Some(SESSION_TOKEN));
    assert_eq!(wire.header("accept"), Some("application/json"));
    assert_eq!(wire.header("anthropic-version"), Some("2023-06-01"));
    for absent in [
        "x-api-key",
        "x-client-request-id",
        "x-claude-code-request-class",
        "x-claude-code-agent-type",
        "x-claude-code-prompt-id",
        "content-encoding",
    ] {
        assert!(wire.header(absent).is_none(), "{absent}");
    }
    assert_eq!(
        wire.header("content-length"),
        Some(wire.body.len().to_string().as_str())
    );
}

#[tokio::test]
async fn the_bedrock_body_is_the_planned_body_without_model_and_stream() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("hi");
    let server = Server::start(vec![profiles(&[], None), answer(&sse)]).await;
    let route = bedrock_route(&server, "us-west-2", "body-1", true);
    let (_, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None);
    let wire = &server.requests()[1];
    let body = String::from_utf8(wire.body.clone()).unwrap();
    let planned = String::from_utf8(planned_body(&route, MODEL)).unwrap();

    // Keys: the planned order, without model and stream, then the version.
    let planned_keys = top_level_keys(&planned);
    let sent_keys = top_level_keys(&body);
    assert_eq!(planned_keys.first().map(String::as_str), Some("model"));
    assert_eq!(planned_keys.last().map(String::as_str), Some("stream"));
    assert!(
        !sent_keys
            .iter()
            .any(|key| key == "model" || key == "stream"),
        "{sent_keys:?}"
    );
    assert_eq!(
        sent_keys.last().map(String::as_str),
        Some("anthropic_version")
    );
    // The betas Bedrock takes in the body sit after context_management and before
    // output_config and diagnostics; the header keeps the rest.
    let mut expected_keys: Vec<String> = planned_keys[1..planned_keys.len() - 1].to_vec();
    let beta_at = expected_keys
        .iter()
        .position(|key| key == "output_config" || key == "diagnostics")
        .unwrap_or(expected_keys.len());
    expected_keys.insert(beta_at, "anthropic_beta".into());
    expected_keys.push("anthropic_version".into());
    assert_eq!(sent_keys, expected_keys);
    // Every other value is the planned one, byte for byte.
    let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mut expected: serde_json::Value = serde_json::from_str(&planned).unwrap();
    expected.as_object_mut().unwrap().remove("model");
    expected.as_object_mut().unwrap().remove("stream");
    expected["anthropic_version"] = "bedrock-2023-05-31".into();
    expected["anthropic_beta"] = sent["anthropic_beta"].clone();
    assert_eq!(sent, expected);
    let betas = wire.header("anthropic-beta").unwrap();
    assert!(
        !betas.contains("interleaved-thinking") && !betas.contains("context-1m"),
        "{betas}"
    );
    assert!(betas.contains("claude-code-20250219"), "{betas}");
    assert_eq!(sent["anthropic_beta"][0], "interleaved-thinking-2025-05-14");
}

/// The top-level keys of a JSON object text, in document order.
fn top_level_keys(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let (mut keys, mut depth, mut at) = (Vec::new(), 0usize, 0usize);
    while at < bytes.len() {
        match bytes[at] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b'"' => {
                let start = at + 1;
                at += 1;
                while bytes[at] != b'"' {
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
                if depth == 1 && bytes.get(at + 1) == Some(&b':') {
                    keys.push(text[start..at].to_owned());
                }
            }
            _ => {}
        }
        at += 1;
    }
    keys
}

#[tokio::test]
async fn the_event_stream_parses_into_the_events_of_the_sse_path() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("same events");
    let server = Server::start(vec![profiles(&[], None), answer(&sse)]).await;
    let route = bedrock_route(&server, "us-east-1", "parse-1", true);
    let (events, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None);

    let direct = super::parse_messages_sse(&sse, &RapidHashSet::new()).unwrap();
    assert_eq!(format!("{events:?}"), format!("{direct:?}"));
    assert!(matches!(
        events.last(),
        Some(RawStreamingChoice::FinalResponse(_))
    ));
    assert_eq!(text_of(&events), "same events");
}

#[tokio::test]
async fn an_in_stream_exception_surfaces_as_the_stream_error() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_text("partial");
    let mut bytes = event_stream_of(&sse);
    bytes.extend(exception_frame(
        "throttlingException",
        "Too many requests, slow down",
    ));
    // Frames after the exception are never read.
    bytes.extend(chunk_frame(r#"{"type":"message_stop"}"#));
    let server = Server::start(vec![
        profiles(&[], None),
        raw_reply(EVENT_STREAM, leak(bytes)),
    ])
    .await;
    let route = bedrock_route(&server, "us-east-1", "exception-1", true);
    let (events, failure) = run(&route, MODEL).await;
    assert_eq!(text_of(&events), "partial");
    let failure = failure.expect("the exception ends the stream with an error");
    assert!(
        failure.contains("throttlingException") && failure.contains("Too many requests, slow down"),
        "{failure}"
    );
}

#[tokio::test]
async fn a_damaged_event_stream_and_a_truncated_one_fail_loud() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("x");
    let mut damaged = event_stream_of(&sse);
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0xff;
    let truncated = event_stream_of(&sse)[..40].to_vec();
    // A stream that ends cleanly before message_stop is also refused.
    let early = chunk_frame(&sse_payloads(&sse)[0]);
    let server = Server::start(vec![
        profiles(&[], None),
        raw_reply(EVENT_STREAM, leak(damaged)),
        raw_reply(EVENT_STREAM, leak(truncated)),
        raw_reply(EVENT_STREAM, leak(early)),
    ])
    .await;
    let route = bedrock_route(&server, "us-east-1", "damaged-1", true);
    for expected in [
        "checksum",
        "ended inside a frame",
        "without an explicit protocol terminal",
    ] {
        let (_, failure) = run(&route, MODEL).await;
        let failure = failure.expect("failure");
        assert!(failure.contains(expected), "{expected}: {failure}");
    }
}

#[tokio::test]
async fn a_response_that_is_not_an_event_stream_is_refused() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![
        profiles(&[], None),
        reply("200 OK", r#"{"type":"message"}"#),
    ])
    .await;
    let route = bedrock_route(&server, "us-east-1", "wrong-type-1", true);
    let (_, failure) = run(&route, MODEL).await;
    let failure = failure.expect("failure");
    assert!(failure.contains("not an event stream"), "{failure}");
}

#[tokio::test]
async fn the_region_prefix_rewrites_the_catalog_model_id() {
    let _guard = lock_fixtures_for_test().await;
    for (region, prefix) in [
        ("us-west-2", "us"),
        ("eu-central-1", "eu"),
        ("ap-southeast-2", "apac"),
        ("ca-central-1", "global"),
        ("us-gov-west-1", "us-gov"),
    ] {
        let sse = crate::claude_messages::sse_fixture_final_text("ok");
        // No listed profile for the model: the catalog id with the region's prefix.
        let server = Server::start(vec![profiles(&[], None), answer(&sse)]).await;
        let route = bedrock_route(&server, region, &format!("prefix-{region}"), true);
        let (_, failure) = run(&route, MODEL).await;
        assert_eq!(failure, None, "{region}");
        assert_eq!(
            server.requests()[1].request_line,
            format!(
                "POST {} HTTP/1.1",
                invoke_target(&format!("{prefix}.anthropic.claude-sonnet-5"))
            ),
            "{region}"
        );
    }
}

#[tokio::test]
async fn discovery_pages_through_the_list_and_is_cached_per_backend() {
    let _guard = lock_fixtures_for_test().await;
    clear_profile_cache_for_test();
    let sse = crate::claude_messages::sse_fixture_final_text("cached");
    let server = Server::start(vec![
        profiles(
            &["us.anthropic.claude-haiku-4-5-20251001-v1:0"],
            Some("token+/="),
        ),
        profiles(&["global.anthropic.claude-sonnet-5"], None),
        answer(&sse),
        answer(&sse),
    ])
    .await;
    let route = bedrock_route(&server, "ca-central-1", "cache-1", true);
    let (_, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None);
    let (_, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None, "the second request reads the cached list");

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        4,
        "two list pages and two invokes, no second listing"
    );
    assert_eq!(
        requests[1].request_line,
        "GET /inference-profiles?nextToken=token%2B%2F%3D&type=SYSTEM_DEFINED HTTP/1.1"
    );
    for page in &requests[..2] {
        let authorization = page.header("authorization").unwrap();
        assert!(
            authorization.contains("/ca-central-1/bedrock/aws4_request"),
            "{authorization}"
        );
    }
    for invoke in &requests[2..] {
        // ca-central-1 prefers a global profile when no ca prefix exists.
        assert_eq!(
            invoke.request_line,
            format!(
                "POST {} HTTP/1.1",
                invoke_target("global.anthropic.claude-sonnet-5")
            )
        );
    }
}

#[tokio::test]
async fn a_listing_that_cannot_be_read_falls_back_to_the_rewritten_id_and_is_not_asked_again() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("fallback");
    let server = Server::start(vec![
        reply(
            "403 Forbidden",
            r#"{"message":"User is not authorized to perform: bedrock:ListInferenceProfiles"}"#,
        ),
        answer(&sse),
        answer(&sse),
    ])
    .await;
    let route = bedrock_route(&server, "us-east-1", "denied-1", true);
    for _ in 0..2 {
        let (events, failure) = run(&route, MODEL).await;
        assert_eq!(failure, None);
        assert_eq!(text_of(&events), "fallback");
    }
    let requests = server.requests();
    // Claude Code's `ED()` keeps the failed listing: one listing, two invokes.
    assert_eq!(requests.len(), 3);
    assert!(requests[0]
        .request_line
        .starts_with("GET /inference-profiles"));
    for invoke in &requests[1..] {
        assert_eq!(
            invoke.request_line,
            format!(
                "POST {} HTTP/1.1",
                invoke_target("us.anthropic.claude-sonnet-5")
            )
        );
    }
}

#[tokio::test]
async fn an_application_inference_profile_arn_is_sent_as_given_without_a_listing() {
    let _guard = lock_fixtures_for_test().await;
    let arn = "arn:aws:bedrock:us-west-2:123456789012:application-inference-profile/abc123";
    let sse = crate::claude_messages::sse_fixture_final_text("arn");
    // Only the invoke is served: a listing would find no server.
    let server = Server::start(vec![answer(&sse)]).await;
    let route = bedrock_route(&server, "us-west-2", "arn-1", true);
    let (events, failure) = run(&route, arn).await;
    assert_eq!(failure, None);
    assert_eq!(text_of(&events), "arn");
    let wire = &server.requests()[0];
    assert_eq!(
        wire.request_line,
        "POST /model/arn:aws:bedrock:us-west-2:123456789012:application-inference-profile%2Fabc123/invoke-with-response-stream HTTP/1.1"
    );
    let scope = Scope {
        region: "us-west-2",
        service: "bedrock",
    };
    assert_eq!(
        wire.header("authorization").unwrap(),
        recomputed_authorization(wire, &scope, Some(SESSION_TOKEN))
    );
    // A model the catalog does not know is likewise sent as given.
    assert_eq!(
        gents_claude::bedrock::rewrite_model_id("us.anthropic.custom-profile", "us"),
        "us.anthropic.custom-profile"
    );
}

#[tokio::test]
async fn a_bearer_token_authenticates_bedrock_without_signing() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("bearer");
    let server = Server::start(vec![profiles(&[], None), answer(&sse)]).await;
    let route = bedrock_route(&server, "us-east-1", "bearer-1", false);
    let (events, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None);
    assert_eq!(text_of(&events), "bearer");
    for wire in server.requests() {
        assert_eq!(
            wire.header("authorization")
                .map(|value| value.replace(BEARER, "<secret>"))
                .as_deref(),
            Some("Bearer <secret>")
        );
        assert!(
            wire.header("x-amz-date").is_none() && wire.header("x-amz-content-sha256").is_none()
        );
    }
    let wire = &server.requests()[1];
    assert!(wire.request_line.contains("/invoke-with-response-stream"));
    assert!(!String::from_utf8_lossy(&wire.body).contains(r#""model":"#));
}

#[tokio::test]
async fn tools_stream_eagerly_only_on_the_default_bedrock_endpoint() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("tools");
    let server = Server::start(vec![
        profiles(&[], None),
        answer(&sse),
        profiles(&[], None),
        answer(&sse),
    ])
    .await;
    let custom = bedrock_route(&server, "us-east-1", "eager-custom", true);
    run(&custom, MODEL).await;
    // The same route marked as the provider's own endpoint.
    let own = ClaudeRoute {
        target: custom.target.at_default_endpoint(true),
        ..bedrock_route(&server, "us-east-1", "eager-own", true)
    };
    run(&own, MODEL).await;
    let requests = server.requests();
    let body = |at: usize| String::from_utf8_lossy(&requests[at].body).into_owned();
    assert!(!body(1).contains("eager_input_streaming"), "{}", body(1));
    assert!(
        body(3).contains(r#""eager_input_streaming":true"#),
        "{}",
        body(3)
    );
}

#[tokio::test]
async fn mantle_signs_the_beta_query_with_its_own_service() {
    let _guard = lock_fixtures_for_test().await;
    let sse = crate::claude_messages::sse_fixture_final_text("mantle");
    let server = Server::start(vec![reply("200 OK", Box::leak(sse.into_boxed_str()))]).await;
    let endpoint = format!("http://127.0.0.1:{}/anthropic", server.port);
    let settings = settings("us-east-1");
    let signer = Arc::new(Signer::new(
        Credentials::new(ACCESS_KEY, SECRET_KEY, None).unwrap(),
    ));
    let route = ClaudeRoute::platform_signed(
        BackendProviderKind::ClaudeMantle,
        &endpoint,
        Some(&settings),
        signer,
    )
    .unwrap();
    let (events, failure) = run(&route, MODEL).await;
    assert_eq!(failure, None);
    assert_eq!(text_of(&events), "mantle");
    let wire = &server.requests()[0];
    assert_eq!(
        wire.request_line,
        "POST /anthropic/v1/messages?beta=true HTTP/1.1"
    );
    // The first-party body, unchanged by the platform.
    assert_eq!(wire.body, planned_body(&route, MODEL));
    let authorization = wire.header("authorization").unwrap();
    assert!(authorization.contains("/us-east-1/bedrock-mantle/aws4_request, SignedHeaders="));
    assert_eq!(signed_headers(authorization), expected_signed(false));
    let scope = Scope {
        region: "us-east-1",
        service: "bedrock-mantle",
    };
    assert_eq!(authorization, recomputed_authorization(wire, &scope, None));
    assert!(wire.header("x-amz-security-token").is_none() && wire.header("x-api-key").is_none());
    assert!(wire.header("x-client-request-id").is_none());
}

#[tokio::test]
async fn claude_platform_on_aws_signs_with_its_service_and_leaves_the_request_id_unsigned() {
    let _guard = lock_fixtures_for_test().await;
    let aws = ClaudePlatform {
        region: Some("us-west-2".into()),
        workspace_id: Some("wrkspc_01".into()),
        ..ClaudePlatform::default()
    };
    let default =
        ClaudePlatform::default_endpoint(BackendProviderKind::ClaudeAnthropicAws, Some(&aws))
            .unwrap()
            .unwrap();
    let on_default = ClaudeRoute::platform_signed(
        BackendProviderKind::ClaudeAnthropicAws,
        &default,
        Some(&aws),
        signer(),
    )
    .unwrap();
    assert!(on_default.client_request_id_header);
    // The default route aimed at the loopback server: the id travels, unsigned.
    let sse = crate::claude_messages::sse_fixture_final_text("aws");
    let server = Server::start(vec![reply("200 OK", Box::leak(sse.into_boxed_str()))]).await;
    let local = ClaudeRoute {
        uri: format!("http://127.0.0.1:{}/v1/messages?beta=true", server.port),
        ..on_default
    };
    let (_, failure) = run(&local, MODEL).await;
    assert_eq!(failure, None);
    let wire = &server.requests()[0];
    assert!(wire.header("x-client-request-id").is_some());
    assert_eq!(wire.header("anthropic-workspace-id"), Some("wrkspc_01"));
    let authorization = wire.header("authorization").unwrap();
    let signed = signed_headers(authorization);
    assert!(
        signed.contains("anthropic-workspace-id") && !signed.contains("x-client-request-id"),
        "{signed}"
    );
    assert!(authorization.contains("/us-west-2/aws-external-anthropic/aws4_request"));
    let scope = Scope {
        region: "us-west-2",
        service: "aws-external-anthropic",
    };
    assert_eq!(
        authorization,
        recomputed_authorization(wire, &scope, Some(SESSION_TOKEN))
    );
}

#[test]
fn a_signing_route_for_another_platform_is_refused() {
    let error = ClaudeRoute::platform_signed(
        BackendProviderKind::ClaudeGateway,
        "https://gateway.example",
        None,
        signer(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("cannot use AWS credentials"),
        "{error}"
    );
}
