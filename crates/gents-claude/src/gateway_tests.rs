use super::*;

#[test]
fn only_same_origin_endpoints_are_taken_else_the_defaults() {
    let base = "https://gw.example.com";
    assert_eq!(
        endpoints(base, None),
        Endpoints {
            device_authorization: "https://gw.example.com/oauth/device_authorization".into(),
            token: "https://gw.example.com/oauth/token".into(),
            revocation: None,
        }
    );
    let metadata = br#"{
        "device_authorization_endpoint": "https://gw.example.com/device",
        "token_endpoint": "https://evil.example.net/token",
        "revocation_endpoint": "https://gw.example.com/revoke"
    }"#;
    assert_eq!(
        endpoints(base, Some(metadata)),
        Endpoints {
            device_authorization: "https://gw.example.com/device".into(),
            token: "https://gw.example.com/oauth/token".into(),
            revocation: Some("https://gw.example.com/revoke".into()),
        }
    );
    // A port is part of the origin; unreadable metadata is no metadata.
    assert_eq!(
        endpoints(
            base,
            Some(br#"{"token_endpoint":"https://gw.example.com:8443/t"}"#)
        )
        .token,
        "https://gw.example.com/oauth/token"
    );
    assert_eq!(endpoints(base, Some(b"<html>")), endpoints(base, None));
}

#[test]
fn the_base_url_is_absolute_http_without_a_trailing_slash() {
    assert_eq!(
        base_url(" https://gw.example.com/ ").unwrap(),
        "https://gw.example.com"
    );
    assert_eq!(base_url("gw.example.com"), Err(GatewayError::Url));
    assert_eq!(base_url("ftp://gw.example.com"), Err(GatewayError::Url));
    assert_eq!(
        metadata_url("https://gw.example.com"),
        "https://gw.example.com/.well-known/oauth-authorization-server"
    );
}

#[test]
fn a_device_code_names_its_page_and_a_positive_interval() {
    let code = parse_device_code(
        br#"{"device_code":"d","user_code":"ABCD-EFGH","verification_uri":"https://gw.example.com/device","expires_in":600}"#,
    )
    .unwrap();
    assert_eq!(code.open_url(), "https://gw.example.com/device");
    assert_eq!(code.interval_secs(), 5);
    let complete = parse_device_code(
        br#"{"device_code":"d","user_code":"u","verification_uri":"v","verification_uri_complete":"v?c=u","expires_in":1,"interval":0}"#,
    )
    .unwrap();
    assert_eq!(complete.open_url(), "v?c=u");
    assert_eq!(complete.interval_secs(), 1);
    assert_eq!(parse_device_code(b"{}"), Err(GatewayError::DeviceCode));
}

#[test]
fn tokens_parse_with_a_lenient_email_and_never_print_the_secrets() {
    let tokens = parse_tokens(
        br#"{"access_token":"jwt-secret","expires_in":3600,"refresh_token":"idp-secret","email":7}"#,
    )
    .unwrap();
    assert_eq!(tokens.expires_in, 3600);
    assert_eq!(tokens.email, None);
    let printed = format!("{tokens:?}");
    assert!(
        !printed.contains("jwt-secret") && !printed.contains("idp-secret"),
        "{printed}"
    );
    let minimal = parse_tokens(br#"{"access_token":"a","expires_in":1}"#).unwrap();
    assert_eq!((minimal.refresh_token, minimal.email), (None, None));
    assert_eq!(
        parse_tokens(br#"{"expires_in":1}"#),
        Err(GatewayError::Tokens)
    );
}

#[test]
fn refused_polls_map_to_their_outcome() {
    for (body, outcome) in [
        (&br#"{"error":"authorization_pending"}"#[..], Poll::Pending),
        (br#"{"error":"slow_down"}"#, Poll::SlowDown),
        (br#"{"error":"expired_token"}"#, Poll::Expired),
        (br#"{"error":"access_denied"}"#, Poll::Denied),
        (
            br#"{"error":"invalid_client"}"#,
            Poll::Failed(Some("invalid_client".into())),
        ),
        (b"bad gateway", Poll::Failed(None)),
    ] {
        assert_eq!(poll_outcome(body), outcome);
    }
}

#[test]
fn emails_drop_controls_and_cut_at_the_cap() {
    assert_eq!(display_email("a@b.c").as_deref(), Some("a@b.c"));
    assert_eq!(display_email("a\u{7}@b").as_deref(), Some("a@b"));
    assert_eq!(display_email("\n"), None);
    let long = "x".repeat(EMAIL_CAP + 3);
    let shown = display_email(&long).unwrap();
    assert_eq!(shown.chars().count(), EMAIL_CAP + 1);
    assert!(shown.ends_with('\u{2026}'));
}

#[test]
fn the_forms_and_agent_are_claude_codes() {
    assert_eq!(device_form(), [("surface", "claude_code")]);
    assert_eq!(
        poll_form("dc"),
        [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", "dc")
        ]
    );
    assert_eq!(
        refresh_form("rt"),
        [("grant_type", "refresh_token"), ("refresh_token", "rt")]
    );
    assert_eq!(
        user_agent(),
        format!("claude-code/{}", crate::profile::VERSION)
    );
}
