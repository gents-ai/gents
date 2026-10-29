use super::*;

const SCOPE_QUERY: &str = "org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload+user%3Aplugins";

fn request<'a>() -> AuthorizeRequest<'a> {
    AuthorizeRequest {
        code_challenge: "CH",
        state: "ST",
        port: 54545,
        manual: false,
        login_with_claude_ai: false,
        inference_only: false,
        org_uuid: None,
        login_hint: None,
        login_method: None,
        client: None,
        also_request_scope: None,
    }
}

#[test]
fn login_scopes_are_the_deduped_union() {
    assert_eq!(
        LOGIN_SCOPES.join(" "),
        "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins"
    );
    assert_eq!(&LOGIN_SCOPES[1..], &REFRESH_SCOPES[..]);
}

#[test]
fn pkce_matches_rfc_7636_appendix_b() {
    let octets = [
        116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37,
        77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
    ];
    let pair = pkce(octets);
    assert_eq!(pair.verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    assert_eq!(
        pair.challenge,
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn state_is_unpadded_base64url() {
    assert_eq!(
        state([0xfb; 32]),
        "-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s"
    );
}

#[test]
fn console_manual_url() {
    let mut r = request();
    r.manual = true;
    assert_eq!(
        authorize_url(&r).unwrap(),
        format!(
            "https://platform.claude.com/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope={SCOPE_QUERY}&code_challenge=CH&code_challenge_method=S256&state=ST"
        )
    );
}

#[test]
fn claude_ai_localhost_url_with_optionals() {
    let mut r = request();
    r.login_with_claude_ai = true;
    r.org_uuid = Some("org-1");
    r.login_hint = Some("a+b@example.com");
    r.login_method = Some("sso");
    assert_eq!(
        authorize_url(&r).unwrap(),
        format!(
            "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&scope={SCOPE_QUERY}&code_challenge=CH&code_challenge_method=S256&state=ST&orgUUID=org-1&login_hint=a%2Bb%40example.com&login_method=sso"
        )
    );
}

#[test]
fn inference_only_and_extra_scope_and_custom_client() {
    let mut r = request();
    r.inference_only = true;
    assert!(authorize_url(&r)
        .unwrap()
        .contains("&scope=user%3Ainference&code_challenge=CH"));
    r.inference_only = false;
    r.also_request_scope = Some("user:extra");
    assert!(authorize_url(&r)
        .unwrap()
        .contains("user%3Aplugins+user%3Aextra&code_challenge"));
    let scopes = vec!["a:b".to_owned(), "c d".to_owned()];
    r.client = Some(ClientOverride {
        client_id: Some("cid"),
        scopes: &scopes,
    });
    let url = authorize_url(&r).unwrap();
    assert!(url.contains("client_id=cid&"));
    assert!(url.contains("&scope=a%3Ab+c+d&"));
}

#[test]
fn exchange_body_is_exact() {
    let mut r = ExchangeRequest {
        code: "C",
        state: "S",
        code_verifier: "V",
        port: 1234,
        manual: false,
        expires_in: None,
        client_id: None,
    };
    let post = exchange_request(&r);
    assert_eq!(post.url, "https://platform.claude.com/v1/oauth/token");
    assert_eq!(post.timeout_ms, 30_000);
    assert_eq!(
        post.body,
        r#"{"grant_type":"authorization_code","code":"C","redirect_uri":"http://localhost:1234/callback","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","code_verifier":"V","state":"S"}"#
    );
    r.manual = true;
    r.expires_in = Some(31536000);
    assert_eq!(
        exchange_request(&r).body,
        r#"{"grant_type":"authorization_code","code":"C","redirect_uri":"https://platform.claude.com/oauth/code/callback","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","code_verifier":"V","state":"S","expires_in":31536000}"#
    );
}

#[test]
fn refresh_body_is_exact() {
    let mut r = RefreshRequest {
        refresh_token: "R",
        scopes: &[],
        expires_in: None,
        client_id: None,
    };
    let post = refresh_request(&r);
    assert_eq!(post.timeout_ms, 30_000);
    assert_eq!(
        post.body,
        r#"{"grant_type":"refresh_token","refresh_token":"R","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","scope":"user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins"}"#
    );
    let granted = vec!["user:inference".to_owned()];
    r.scopes = &granted;
    r.expires_in = Some(60);
    r.client_id = Some("cid");
    assert_eq!(
        refresh_request(&r).body,
        r#"{"grant_type":"refresh_token","refresh_token":"R","client_id":"cid","scope":"user:inference","expires_in":60}"#
    );
}

#[test]
fn revoke_body_is_exact() {
    let post = revoke_request("R", None);
    assert_eq!(
        post.url,
        "https://platform.claude.com/v1/oauth/token/revoke"
    );
    assert_eq!(post.timeout_ms, 5_000);
    assert_eq!(
        post.body,
        r#"{"token":"R","token_type_hint":"refresh_token","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e"}"#
    );
}

#[test]
fn parses_a_full_response() {
    let body = br#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"refresh_token_expires_in":86400,"scope":"user:profile  user:inference","token_type":"Bearer","account":{"uuid":"u","email_address":"e@x"},"organization":{"uuid":"o"}}"#;
    let tokens = parse_token_response(body, 1_000, Some("OLD")).unwrap();
    assert_eq!(tokens.access_token, "AT");
    assert_eq!(tokens.refresh_token.as_deref(), Some("RT"));
    assert_eq!(tokens.expires_at_ms, 3_601_000);
    assert_eq!(tokens.refresh_token_expires_at_ms, Some(86_401_000));
    assert_eq!(tokens.scopes, ["user:profile", "user:inference"]);
    assert_eq!(tokens.account.unwrap().uuid, "u");
    assert_eq!(tokens.organization_uuid.as_deref(), Some("o"));
}

#[test]
fn parses_a_minimal_response_and_keeps_the_old_refresh_token() {
    let body = br#"{"access_token":"AT","expires_in":10,"token_type":"Bearer"}"#;
    let tokens = parse_token_response(body, 5, Some("OLD")).unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("OLD"));
    assert_eq!(tokens.expires_at_ms, 10_005);
    assert_eq!(tokens.refresh_token_expires_at_ms, None);
    assert!(tokens.scopes.is_empty());
    assert!(tokens.account.is_none() && tokens.organization_uuid.is_none());
    assert_eq!(
        parse_token_response(body, 5, None).unwrap().refresh_token,
        None
    );
}

#[test]
fn malformed_and_overflowing_responses_fail_without_leaking() {
    let err = parse_token_response(br#"{"access_token":"SECRET","expires_in":"x"}"#, 0, None)
        .unwrap_err();
    assert_eq!(err, OAuthError::Malformed("fields"));
    assert!(!err.to_string().contains("SECRET"));
    assert_eq!(
        parse_token_response(b"{", 0, None),
        Err(OAuthError::Malformed("truncated"))
    );
    assert_eq!(
        parse_token_response(
            br#"{"access_token":"a","expires_in":18446744073709551615}"#,
            0,
            None
        ),
        Err(OAuthError::ExpiryOutOfRange)
    );
}

#[test]
fn failure_classification_table() {
    let hold = br#"{"error":"invalid_grant","error_description":"account_on_hold"}"#;
    let denied = br#"{"error":"access_denied","error_description":"account_on_hold","error_uri":"https://claude.ai/x"}"#;
    let wrong_desc = br#"{"error":"invalid_grant","error_description":"other"}"#;
    let wrong_code = br#"{"error":"invalid_client","error_description":"account_on_hold"}"#;
    let grant = br#"{"error":"invalid_grant"}"#;
    let typed = br#"{"error":{"type":"invalid_grant","message":"x"}}"#;
    let scope = br#"{"error":"invalid_scope"}"#;
    let string_body =
        br#""{\"error\":\"invalid_grant\",\"error_description\":\"account_on_hold\"}""#;
    use RefreshFailure::*;
    for (status, body, expected) in [
        (Some(400), &hold[..], AccountOnHold),
        (Some(401), hold, AccountOnHold),
        (Some(403), hold, AccountOnHold),
        (Some(403), denied, AccountOnHold),
        (Some(400), string_body, AccountOnHold),
        (Some(500), hold, Transient),
        (Some(400), wrong_desc, SignedOut),
        (Some(401), wrong_desc, SignedOut),
        (Some(400), wrong_code, Rejected),
        (Some(400), grant, SignedOut),
        (Some(401), typed, SignedOut),
        (Some(403), grant, Rejected),
        (Some(400), scope, InvalidScope),
        (Some(401), scope, Rejected),
        (Some(400), b"not json", Rejected),
        (Some(404), b"", Rejected),
        (Some(408), b"", Transient),
        (Some(409), b"", Transient),
        (Some(429), b"", Transient),
        (Some(500), b"", Transient),
        (Some(503), b"", Transient),
        (None, b"", Transient),
    ] {
        assert_eq!(
            classify_failure(status, body),
            expected,
            "{status:?} {}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn debug_output_redacts_secrets() {
    let post = exchange_request(&ExchangeRequest {
        code: "CODE-SECRET",
        state: "S",
        code_verifier: "VERIFIER-SECRET",
        port: 1,
        manual: false,
        expires_in: None,
        client_id: None,
    });
    let pair = pkce([7; 32]);
    let tokens = parse_token_response(
        br#"{"access_token":"AT-SECRET","refresh_token":"RT-SECRET","expires_in":1,"account":{"uuid":"u","email_address":"me@example.com"}}"#,
        0,
        None,
    )
    .unwrap();
    let all = format!("{post:?} {pair:?} {tokens:?}");
    for secret in [
        "CODE-SECRET",
        "VERIFIER-SECRET",
        &pair.verifier,
        "AT-SECRET",
        "RT-SECRET",
        "me@example.com",
    ] {
        assert!(!all.contains(secret), "{secret} leaked");
    }
    assert!(all.contains(&pair.challenge));
}
