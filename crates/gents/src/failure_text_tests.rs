use super::*;

#[derive(Debug)]
struct Layer {
    text: String,
    source: Option<Box<Layer>>,
}

impl fmt::Display for Layer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl std::error::Error for Layer {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

#[test]
fn a_100_kib_provider_body_is_capped_per_cause_and_overall() {
    let body = format!(
        r#"{{"error":{{"message":"prompt is too long: 1001685 tokens > 1000000 maximum","echo":"{}"}}}}"#,
        "x".repeat(100 * 1024)
    );
    let error = anyhow::Error::from(rig::http_client::Error::InvalidStatusCodeWithMessage(
        "400".parse().unwrap(),
        body,
    ))
    .context("ProviderError: inference failed")
    .context("projecting compactable provider messages");

    let reason = bounded_error_chain(error.as_ref());
    assert!(reason.len() <= FAILURE_TEXT_MAX_BYTES, "{}", reason.len());
    assert!(reason.starts_with(
        "projecting compactable provider messages: ProviderError: inference failed: "
    ));
    assert!(
        reason.contains("prompt is too long: 1001685 tokens"),
        "the cause leads"
    );
    assert!(reason.ends_with(TRUNCATION_MARKER));
    for cause in reason.split(": ") {
        assert!(cause.len() <= FAILURE_CAUSE_MAX_BYTES, "{}", cause.len());
    }
    assert_eq!(
        BoundedErrorChain(error.as_ref()).to_string(),
        reason,
        "logs get the same rendering"
    );
}

#[test]
fn the_whole_chain_is_capped_even_when_every_cause_fits() {
    let mut error = None;
    for index in 0..10 {
        error = Some(Box::new(Layer {
            text: format!("cause {index} {}", "y".repeat(900)),
            source: error,
        }));
    }
    let reason = bounded_error_chain(error.as_deref().unwrap());
    assert!(reason.len() <= FAILURE_TEXT_MAX_BYTES);
    assert!(reason.starts_with("cause 9 "));
    assert!(reason.ends_with(TRUNCATION_MARKER));
}

#[test]
fn truncation_respects_character_boundaries() {
    let text = "é".repeat(2_000);
    let cut = truncate(&text, FAILURE_CAUSE_MAX_BYTES);
    assert!(cut.len() <= FAILURE_CAUSE_MAX_BYTES);
    assert!(cut.ends_with(TRUNCATION_MARKER));
    assert_eq!(truncate("short", FAILURE_CAUSE_MAX_BYTES), "short");
}

#[test]
fn credentials_and_token_bearing_urls_never_survive() {
    let error = Layer {
        text: "error sending request for url \
            (https://user:pw@generativelanguage.googleapis.com/v1/models:generate?key=AIzaSyDUMMYDUMMYDUMMY&alt=sse)"
            .to_string(),
        source: Some(Box::new(Layer {
            text: r#"echoed headers: Authorization: Bearer sk-ant-api03-SECRETSECRETSECRET, x-api-key: sk-proj-ABCDEFGHIJKLMN, {"api_key":"plain-secret-value","access_token":"tok_123456"} Cookie: session=abc123"#
                .to_string(),
            source: None,
        })),
    };
    let reason = bounded_error_chain(&error);
    for secret in [
        "user:pw",
        "AIzaSyDUMMY",
        "key=",
        "alt=sse",
        "sk-ant-api03",
        "SECRETSECRET",
        "sk-proj",
        "ABCDEFGHIJKLMN",
        "plain-secret-value",
        "tok_123456",
        "session=abc123",
    ] {
        assert!(!reason.contains(secret), "{secret} leaked: {reason}");
    }
    assert!(
        reason.contains("https://generativelanguage.googleapis.com/v1/models:generate"),
        "the endpoint without its query stays: {reason}"
    );
    assert!(reason.contains("Authorization: [REDACTED]"), "{reason}");
}

#[test]
fn provider_rejection_errors_carry_no_request_headers() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer sk-ant-api03-HEADERSECRET".parse().unwrap(),
    );
    headers.insert("x-api-key", "sk-proj-HEADERSECRET".parse().unwrap());
    headers.insert("retry-after", "30".parse().unwrap());
    let error = crate::provider_http::rejected_response_error(
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        &headers,
        r#"{"error":{"type":"rate_limit_error"}}"#,
    );
    let rendered = error.to_string();
    assert!(!rendered.contains("HEADERSECRET"), "{rendered}");
    assert!(rendered.contains("rate_limit_error"));
}
