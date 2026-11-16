//! Google key files against a loopback token endpoint: the exact form each
//! grant posts, the RS256 assertion verified with the key's public half, the
//! cache, the eager refresh, invalidation and the refusal text. Keys are
//! generated per test, so no private key is stored in the repository.

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair, UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};

use super::super::wire_tests::{reply, Server, Wire};
use super::*;

const TOKEN: &str = r#"{"access_token":"ya29.minted","expires_in":3599,"token_type":"Bearer"}"#;

/// A fresh service account key file and the key it holds.
fn service_account(email: &str) -> (String, RsaKeyPair) {
    let key = RsaKeyPair::generate(KeySize::Rsa2048).unwrap();
    let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&key).unwrap();
    let body = STANDARD.encode(der.as_ref());
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    );
    let json = serde_json::json!({
        "type": "service_account",
        "project_id": "p",
        "private_key_id": "kid",
        "private_key": pem,
        "client_email": email,
        "token_uri": "https://oauth2.googleapis.com/token",
    })
    .to_string();
    (json, key)
}

fn authorized_user(refresh_token: &str) -> String {
    serde_json::json!({
        "type": "authorized_user",
        "client_id": "client.apps.googleusercontent.com",
        "client_secret": "user-secret",
        "refresh_token": refresh_token,
        "quota_project_id": "billing-project",
    })
    .to_string()
}

fn source(json: &str, server: &Server) -> GoogleTokenSource {
    GoogleTokenSource::at(
        Arc::new(GoogleCredentials::parse(json).unwrap()),
        &format!("http://127.0.0.1:{}/token", server.port),
    )
}

fn form(wire: &Wire) -> Vec<(String, String)> {
    gents_claude::sigv4::query_pairs(std::str::from_utf8(&wire.body).unwrap())
}

#[tokio::test]
async fn a_service_account_exchanges_a_signed_assertion_once() {
    let (json, key) = service_account("robot@p.iam.gserviceaccount.com");
    let server = Server::start(vec![reply("200 OK", TOKEN)]).await;
    let tokens = source(&json, &server);
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.minted");
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.minted");

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "the second call reads the cache");
    let wire = &requests[0];
    assert_eq!(wire.request_line, "POST /token HTTP/1.1");
    assert_eq!(
        wire.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    let fields = form(wire);
    assert_eq!(fields[0], ("grant_type".into(), JWT_BEARER.into()));
    assert_eq!(fields[1].0, "assertion");
    let parts: Vec<&str> = fields[1].1.split('.').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(
        URL_SAFE_NO_PAD.decode(parts[0]).unwrap(),
        br#"{"alg":"RS256"}"#
    );
    let claims = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&claims).unwrap();
    let iat = value["iat"].as_u64().unwrap();
    assert_eq!(
        claims,
        format!(
            r#"{{"iss":"robot@p.iam.gserviceaccount.com","scope":"{CLOUD_PLATFORM_SCOPE}","aud":"https://oauth2.googleapis.com/token","exp":{},"iat":{iat}}}"#,
            iat + 3600
        )
    );
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public_key().as_ref())
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
        .expect("the assertion verifies with the key's public half");
}

#[tokio::test]
async fn an_authorized_user_trades_its_refresh_token_and_names_its_quota_project() {
    let json = authorized_user("refresh-1");
    let server = Server::start(vec![reply("200 OK", TOKEN)]).await;
    let tokens = source(&json, &server);
    assert_eq!(tokens.credentials.quota_project(), Some("billing-project"));
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.minted");
    let fields = form(&server.requests()[0]);
    let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["refresh_token", "client_id", "client_secret", "grant_type"]
    );
    assert_eq!(fields[0].1, "refresh-1");
    assert_eq!(fields[3].1, "refresh_token");
}

#[tokio::test]
async fn a_refused_exchange_reports_googles_code_and_the_next_call_retries() {
    let json = authorized_user("refresh-refused");
    let server = Server::start(vec![
        reply(
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        ),
        reply("200 OK", TOKEN),
    ])
    .await;
    let tokens = source(&json, &server);
    let error = tokens.access_token().await.unwrap_err().to_string();
    assert_eq!(
        error,
        "Google refused the token exchange: invalid_grant: Token has been expired or revoked."
    );
    assert!(!error.contains("refresh-refused") && !error.contains("user-secret"));
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.minted");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn a_token_inside_the_eager_refresh_window_and_an_invalidated_one_are_replaced() {
    let json = authorized_user("refresh-short");
    let server = Server::start(vec![
        reply(
            "200 OK",
            r#"{"access_token":"ya29.short","expires_in":120}"#,
        ),
        reply("200 OK", TOKEN),
        reply(
            "200 OK",
            r#"{"access_token":"ya29.after-401","expires_in":3599}"#,
        ),
    ])
    .await;
    let tokens = source(&json, &server);
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.short");
    // Two minutes is inside the five-minute eager window: replaced at once.
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.minted");
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.minted");
    BearerSource::invalidate(&tokens).await;
    assert_eq!(tokens.access_token().await.unwrap(), "ya29.after-401");
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn key_files_that_cannot_mint_fail_with_the_reason_and_no_secret() {
    let parse = |json: &str| GoogleCredentials::parse(json).unwrap_err();
    assert_eq!(
        parse(r#"{"type":"service_account","private_key":"hunter2"#),
        GoogleCredentialError::NotJson {
            line: 1,
            column: 48
        }
    );
    assert_eq!(
        parse(r#"{"type":"authorized_user","client_id":"c","refresh_token":"r"}"#),
        GoogleCredentialError::Missing {
            kind: "authorized user",
            field: "client_secret"
        }
    );
    assert_eq!(
        parse(r#"{"type":"service_account","client_email":"e","private_key":"hunter2"}"#),
        GoogleCredentialError::PrivateKey
    );
    assert_eq!(
        parse(r#"{"client_email":"e"}"#),
        GoogleCredentialError::Missing {
            kind: "key file",
            field: "type"
        }
    );
    let external = parse(r#"{"type":"external_account","audience":"a"}"#);
    assert_eq!(
        external.to_string(),
        "Google external_account credentials are not supported; use a service account key or an authorized user file"
    );
    let (json, _) = service_account("robot@p.iam.gserviceaccount.com");
    let printed = format!("{:?}", GoogleCredentials::parse(&json).unwrap());
    assert!(
        printed.contains("robot@p.iam.gserviceaccount.com"),
        "{printed}"
    );
    assert!(!printed.contains("PRIVATE KEY"), "{printed}");
    let user = format!(
        "{:?}",
        GoogleCredentials::parse(&authorized_user("refresh-secret")).unwrap()
    );
    assert!(
        !user.contains("refresh-secret") && !user.contains("user-secret"),
        "{user}"
    );
}
