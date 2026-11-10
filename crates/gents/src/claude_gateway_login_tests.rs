//! A loopback gateway: metadata, device authorization and token endpoints with
//! a scripted sequence of token answers, recording each form it receives.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use rapidhash::RapidHashMap;
use serde_json::{json, Value};

use super::*;

/// The endpoint each form went to, with its fields.
type Forms = Vec<(String, RapidHashMap<String, String>)>;

#[derive(Clone, Default)]
struct Gateway {
    /// Token answers served in order; the last one repeats.
    answers: Arc<Mutex<Vec<(StatusCode, Value)>>>,
    forms: Arc<Mutex<Forms>>,
    advertise: bool,
}

async fn metadata(State(gateway): State<Gateway>) -> (StatusCode, Json<Value>) {
    if !gateway.advertise {
        return (StatusCode::NOT_FOUND, Json(json!({})));
    }
    (
        StatusCode::OK,
        Json(json!({
            "device_authorization_endpoint": "http://127.0.0.1:1/elsewhere",
            "token_endpoint": "/relative-is-not-an-origin",
        })),
    )
}

async fn device(
    State(gateway): State<Gateway>,
    Form(form): Form<RapidHashMap<String, String>>,
) -> Json<Value> {
    gateway.forms.lock().unwrap().push(("device".into(), form));
    Json(json!({
        "device_code": "device-1",
        "user_code": "ABCD-EFGH",
        "verification_uri": "https://gw.example/activate",
        "expires_in": 60,
        "interval": 1,
    }))
}

async fn token(
    State(gateway): State<Gateway>,
    Form(form): Form<RapidHashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    gateway.forms.lock().unwrap().push(("token".into(), form));
    let mut answers = gateway.answers.lock().unwrap();
    let answer = if answers.len() > 1 {
        answers.remove(0)
    } else {
        answers[0].clone()
    };
    (answer.0, Json(answer.1))
}

async fn serve(gateway: Gateway) -> String {
    let app = Router::new()
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/oauth/device_authorization", post(device))
        .route("/oauth/token", post(token))
        .with_state(gateway);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn gateway(answers: Vec<(StatusCode, Value)>, advertise: bool) -> Gateway {
    Gateway {
        answers: Arc::new(Mutex::new(answers)),
        advertise,
        ..Gateway::default()
    }
}

fn tokens(refresh: Option<&str>) -> Value {
    json!({
        "access_token": "gateway-jwt",
        "expires_in": 3600,
        "refresh_token": refresh,
        "email": "dev@example.com",
    })
}

#[tokio::test]
async fn a_device_sign_in_polls_past_pending_and_stores_the_gateway_account() {
    let state = gateway(
        vec![
            (
                StatusCode::BAD_REQUEST,
                json!({"error": "authorization_pending"}),
            ),
            (StatusCode::OK, tokens(Some("idp-refresh"))),
        ],
        // Foreign and non-URL endpoints are ignored for the defaults.
        true,
    );
    let base = serve(state.clone()).await;
    let http = gateway_http().unwrap();
    let endpoints = discover(&http, &base).await;
    assert_eq!(endpoints.token, format!("{base}/oauth/token"));
    let code = request_device_code(&http, &endpoints).await.unwrap();
    assert_eq!(code.user_code, "ABCD-EFGH");
    let signed_in = poll_tokens(&http, &endpoints, &code).await.unwrap();
    let now = Utc::now();
    let credential = credential_from_tokens("did:key:z6MkGateway", &base, &signed_in, now);
    assert_eq!(credential.provider, CLAUDE_GATEWAY_PROVIDER);
    assert_eq!(credential.access_token, "gateway-jwt");
    assert_eq!(credential.refresh_token, "idp-refresh");
    assert_eq!(credential.account_id.as_deref(), Some(base.as_str()));
    assert_eq!(credential.label.as_deref(), Some("dev@example.com"));
    assert_eq!(
        credential.access_token_expires_at,
        now + chrono::Duration::seconds(3600)
    );

    let forms = state.forms.lock().unwrap().clone();
    assert_eq!(forms[0].0, "device");
    assert_eq!(
        forms[0].1.get("surface").map(String::as_str),
        Some("claude_code")
    );
    assert_eq!(forms.len(), 3, "the device request and two polls");
    for (_, poll) in &forms[1..] {
        assert_eq!(
            poll.get("grant_type").map(String::as_str),
            Some("urn:ietf:params:oauth:grant-type:device_code")
        );
        assert_eq!(
            poll.get("device_code").map(String::as_str),
            Some("device-1")
        );
    }
}

#[tokio::test]
async fn a_denied_sign_in_says_so() {
    let base = serve(gateway(
        vec![(StatusCode::BAD_REQUEST, json!({"error": "access_denied"}))],
        false,
    ))
    .await;
    let http = gateway_http().unwrap();
    let endpoints = discover(&http, &base).await;
    let code = request_device_code(&http, &endpoints).await.unwrap();
    let error = poll_tokens(&http, &endpoints, &code).await.unwrap_err();
    assert_eq!(error.to_string(), "Sign-in was denied in the browser.");
}

#[tokio::test]
async fn a_refresh_keeps_the_refresh_token_unless_rotated_and_invalid_grant_signs_out() {
    let state = gateway(
        vec![
            (StatusCode::OK, tokens(None)),
            (StatusCode::OK, tokens(Some("rotated"))),
            (StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"})),
        ],
        false,
    );
    let base = serve(state.clone()).await;
    let http = gateway_http().unwrap();
    let kept = refresh_gateway_token(Some(&base), "idp-1", &http)
        .await
        .unwrap();
    assert_eq!(kept.access_token, "gateway-jwt");
    assert_eq!(kept.refresh_token, "idp-1");
    let rotated = refresh_gateway_token(Some(&base), "idp-1", &http)
        .await
        .unwrap();
    assert_eq!(rotated.refresh_token, "rotated");
    assert_eq!(
        refresh_gateway_token(Some(&base), "idp-1", &http)
            .await
            .err(),
        Some(OAuthAuthProblem::Expired)
    );
    let forms = state.forms.lock().unwrap().clone();
    assert_eq!(
        forms[0].1.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        forms[0].1.get("refresh_token").map(String::as_str),
        Some("idp-1")
    );
    // No refresh token: signed out without asking the gateway.
    assert_eq!(
        refresh_gateway_token(Some(&base), "", &http).await.err(),
        Some(OAuthAuthProblem::Expired)
    );
    assert_eq!(state.forms.lock().unwrap().len(), 3);
}
