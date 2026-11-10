//! Loopback tests of the Google routes: Vertex AI sends the `streamRawPredict`
//! request with the model in the path, the rewritten body, the minted bearer
//! and the key file's quota project; Claude Platform on Google Cloud sends the
//! first-party body under its workspace base with the minted bearer only. A
//! rejected token is minted again and the request resent.

use std::sync::Arc;

use futures::StreamExt;
use gents_claude::vertex as vertex_rules;
use gents_loop::backend_provider::BackendProviderKind;
use gents_loop::claude_messages_body::plan_claude_request_for;
use rapidhash::{HashSetExt, RapidHashSet};
use rig::streaming::RawStreamingChoice;

use super::lock_fixtures_for_test;
use super::platform_wire_tests::{expected_body, expected_names, names, settings, sse_body, MODEL};
use super::route::ClaudeRoute;
use super::send::stream_messages_routed;
use super::tests::echo_request;
use super::wire_tests::{reply, Reply, Server, Wire};
use super::{GoogleCredentials, GoogleTokenSource};
use crate::inference::clients::claude_subscription::{AccountIdentity, ApiKeyBearer};

const MINTED: Reply = reply(
    "200 OK",
    r#"{"access_token":"ya29.vertex","expires_in":3599,"token_type":"Bearer"}"#,
);

/// An authorized user file unique to `test`, so the process-wide token cache
/// never serves one test's token to another.
fn key_file(test: &str) -> Arc<GoogleCredentials> {
    Arc::new(
        GoogleCredentials::parse(
            &serde_json::json!({
                "type": "authorized_user",
                "client_id": "client.apps.googleusercontent.com",
                "client_secret": "user-secret",
                "refresh_token": format!("refresh-{test}"),
                "quota_project_id": "billing-project",
            })
            .to_string(),
        )
        .unwrap(),
    )
}

fn vertex_route(server: &Server, credentials: &GoogleCredentials) -> ClaudeRoute {
    let vertex = settings(|s| {
        s.project = Some("proj".into());
        s.location = Some("us-east5".into());
    });
    ClaudeRoute::platform(
        BackendProviderKind::ClaudeVertex,
        &format!("http://127.0.0.1:{}/v1", server.port),
        Some(&vertex),
        "",
    )
    .unwrap()
    .with_quota_project(credentials.quota_project())
}

/// Sends the echo request with tokens minted at the loopback server and
/// returns the streamed text, or the first failure.
async fn send(
    route: &ClaudeRoute,
    server: &Server,
    credentials: Arc<GoogleCredentials>,
) -> Result<String, String> {
    let account = AccountIdentity::of_platform("did:key:z6MkPlatform", "backend-1").context(false);
    let bearer = ApiKeyBearer::google(GoogleTokenSource::at(
        credentials,
        &format!("http://127.0.0.1:{}/token", server.port),
    ));
    let stream = stream_messages_routed(
        route,
        MODEL,
        &echo_request(),
        &account,
        RapidHashSet::new(),
        &bearer,
        None,
    )
    .await
    .map_err(|error| error.to_string())?;
    futures::pin_mut!(stream);
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        match item.map_err(|error| error.to_string())? {
            RawStreamingChoice::Message(chunk) => text.push_str(&chunk),
            RawStreamingChoice::FinalResponse(_) => break,
            _ => {}
        }
    }
    Ok(text)
}

fn planned_model(route: &ClaudeRoute) -> String {
    let account = AccountIdentity::of_platform("did:key:z6MkPlatform", "backend-1").context(false);
    plan_claude_request_for(MODEL, &echo_request(), &account, route.target)
        .unwrap()
        .body
        .model
}

fn assert_vertex_request(wire: &Wire, route: &ClaudeRoute) {
    assert_eq!(
        wire.request_line,
        format!(
            "POST /v1/projects/proj/locations/us-east5/publishers/anthropic/models/{}:streamRawPredict HTTP/1.1",
            planned_model(route)
        )
    );
    assert_eq!(wire.header("authorization"), Some("Bearer ya29.vertex"));
    assert_eq!(wire.header("x-goog-user-project"), Some("billing-project"));
    assert!(wire.header("x-client-request-id").is_none());
    assert_eq!(
        names(wire),
        expected_names(&["Authorization", "x-goog-user-project"])
    );
    let body = vertex_rules::transform_body(&expected_body(route)).unwrap();
    assert_eq!(wire.body, body);
    let text = String::from_utf8_lossy(&wire.body);
    assert!(
        text.ends_with(r#","stream":true,"anthropic_version":"vertex-2023-10-16"}"#),
        "{text}"
    );
    assert!(!text.contains(r#""model":"#), "{text}");
    // Vertex carries the attribution's `cch` placeholder, as Claude Code's does.
    assert!(text.contains("cch=00000;"), "{text}");
}

#[tokio::test]
async fn vertex_sends_the_predict_request_with_the_minted_token_and_quota_project() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![MINTED, reply("200 OK", sse_body())]).await;
    let credentials = key_file("vertex-send");
    let route = vertex_route(&server, &credentials);
    assert_eq!(
        send(&route, &server, credentials).await.as_deref(),
        Ok("platform hello")
    );
    let requests = server.requests();
    assert_eq!(requests[0].request_line, "POST /token HTTP/1.1");
    assert_vertex_request(&requests[1], &route);
}

#[tokio::test]
async fn a_rejected_google_token_is_minted_again_and_the_request_resent() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![
        reply(
            "200 OK",
            r#"{"access_token":"ya29.stale","expires_in":3599}"#,
        ),
        reply(
            "401 Unauthorized",
            r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#,
        ),
        MINTED,
        reply("200 OK", sse_body()),
    ])
    .await;
    let credentials = key_file("vertex-401");
    let route = vertex_route(&server, &credentials);
    assert_eq!(
        send(&route, &server, credentials).await.as_deref(),
        Ok("platform hello")
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1].header("authorization"),
        Some("Bearer ya29.stale")
    );
    assert_eq!(requests[2].request_line, "POST /token HTTP/1.1");
    assert_vertex_request(&requests[3], &route);
}

#[tokio::test]
async fn a_failed_token_exchange_fails_the_request_with_googles_reason() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![reply(
        "400 Bad Request",
        r#"{"error":"invalid_grant","error_description":"Bad Request"}"#,
    )])
    .await;
    let credentials = key_file("vertex-refused");
    let route = vertex_route(&server, &credentials);
    let error = send(&route, &server, credentials).await.unwrap_err();
    assert!(error.contains("invalid_grant: Bad Request"), "{error}");
    assert!(
        !error.contains("user-secret") && !error.contains("refresh-"),
        "{error}"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn google_cloud_sends_the_first_party_body_under_its_workspace_base() {
    let _guard = lock_fixtures_for_test().await;
    let server = Server::start(vec![MINTED, reply("200 OK", sse_body())]).await;
    let credentials = key_file("google-cloud");
    let base = "/v1/projects/proj/locations/global/workspaces/ws/invoke";
    let cloud = settings(|s| {
        s.project = Some("proj".into());
        s.workspace_id = Some("ws".into());
    });
    let route = ClaudeRoute::platform(
        BackendProviderKind::ClaudeGoogleCloud,
        &format!("http://127.0.0.1:{}{base}", server.port),
        Some(&cloud),
        "",
    )
    .unwrap()
    .with_quota_project(credentials.quota_project());
    assert_eq!(
        send(&route, &server, credentials).await.as_deref(),
        Ok("platform hello")
    );
    let wire = &server.requests()[1];
    assert_eq!(
        wire.request_line,
        format!("POST {base}/v1/messages?beta=true HTTP/1.1")
    );
    assert_eq!(wire.header("authorization"), Some("Bearer ya29.vertex"));
    // The class adds only the bearer; the quota project is a Vertex header.
    assert!(wire.header("x-goog-user-project").is_none());
    assert!(wire.header("x-client-request-id").is_none());
    assert_eq!(names(wire), expected_names(&["Authorization"]));
    assert_eq!(wire.body, expected_body(&route));
    let text = String::from_utf8_lossy(&wire.body);
    assert!(!text.contains("cch=00000"), "{text}");
}
