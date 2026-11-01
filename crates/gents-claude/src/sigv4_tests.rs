//! Signer tests. The first group are cases of the published AWS Signature
//! Version 4 test suite (`aws-sig-v4-test-suite`: get-vanilla, post-vanilla,
//! get-vanilla-query-order-key-case, get-vanilla-empty-query-key, get-relative)
//! with the suite's credentials (AKIDEXAMPLE), region us-east-1, service
//! `service` and time 2015-08-30T12:36:00Z; the suite carries no
//! `x-amz-content-sha256` header, so these use `with_checksum(false)`. The
//! second group is a request built the way Claude Code builds a Bedrock and a
//! Mantle request; its expected signatures come from an independent
//! implementation of the published algorithm (Python `hmac` and `hashlib`), not
//! from this module.

use super::*;

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const SUITE_TIME: u64 = 1_440_938_160;
const SUITE_SCOPE: Scope<'static> = Scope {
    region: "us-east-1",
    service: "service",
};

fn suite_signer() -> Signer {
    Signer::new(Credentials::new("AKIDEXAMPLE", SECRET, None).unwrap()).with_checksum(false)
}

fn authorization(added: &[(&'static str, HeaderValue)]) -> String {
    added
        .iter()
        .find(|(name, _)| *name == "authorization")
        .map(|(_, value)| value.value.clone())
        .expect("authorization")
}

fn suite_case(method: &str, path: &str, query: &[(&str, &str)], body: &[u8]) -> String {
    let added = suite_signer()
        .sign(
            &Request {
                method,
                path,
                query,
                host: "example.amazonaws.com",
                headers: &[],
                body,
            },
            &SUITE_SCOPE,
            SUITE_TIME,
        )
        .unwrap();
    authorization(&added)
}

fn suite_expected(signature: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature={signature}"
    )
}

#[test]
fn the_published_get_vanilla_case_signs() {
    assert_eq!(
        suite_case("GET", "/", &[], b""),
        suite_expected("5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31")
    );
}

#[test]
fn the_published_post_vanilla_case_signs() {
    assert_eq!(
        suite_case("POST", "/", &[], b""),
        suite_expected("5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b")
    );
}

#[test]
fn the_published_query_order_key_case_signs() {
    assert_eq!(
        suite_case(
            "GET",
            "/",
            &[("Param2", "value2"), ("Param1", "value1")],
            b""
        ),
        suite_expected("b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500")
    );
}

#[test]
fn the_published_empty_query_key_case_signs() {
    assert_eq!(
        suite_case("GET", "/", &[("Param1", "value1")], b""),
        suite_expected("a67d582fa61cc504c4bae71f336f98b97f1ea3c7a6bfe1b6e45aec72011b9aeb")
    );
}

#[test]
fn the_published_relative_path_case_normalizes_to_the_root() {
    assert_eq!(
        suite_case("GET", "/example/..", &[], b""),
        suite_expected("5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31")
    );
}

const CLAUDE_TIME: u64 = 1_793_448_000;
const CLAUDE_BODY: &[u8] = br#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":8,"anthropic_version":"bedrock-2023-05-31"}"#;
const CLAUDE_HEADERS: [(&str, &str); 8] = [
    ("Accept", "application/json"),
    ("X-Stainless-Retry-Count", "0"),
    ("anthropic-version", "2023-06-01"),
    (
        "anthropic-beta",
        "claude-code-20250219,context-management-2025-06-27",
    ),
    ("Content-Type", "application/json"),
    ("x-app", "cli"),
    ("User-Agent", "claude-cli/2.1.311 (external, cli)"),
    (
        "X-Claude-Code-Session-Id",
        "11111111-2222-4333-8444-555555555555",
    ),
];
const SIGNED: &str = "accept;anthropic-beta;anthropic-version;content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-app;x-claude-code-session-id;x-stainless-retry-count";
const BODY_SHA256: &str = "01f66ca09e019aec173f320db67e382b5732e3907dffaa4371d0cf7599197eb4";

#[test]
fn a_bedrock_request_built_the_way_claude_code_builds_it_signs() {
    let signer =
        Signer::new(Credentials::new("AKIDEXAMPLE", SECRET, Some("SESSIONTOKEN")).unwrap());
    let added = signer
        .sign(
            &Request {
                method: "POST",
                path: "/model/arn:aws:bedrock:us-west-2:123456789012:application-inference-profile%2Fabc123/invoke-with-response-stream",
                query: &[],
                host: "bedrock-runtime.us-west-2.amazonaws.com",
                headers: &CLAUDE_HEADERS,
                body: CLAUDE_BODY,
            },
            &Scope {
                region: "us-west-2",
                service: "bedrock",
            },
            CLAUDE_TIME,
        )
        .unwrap();
    let names: Vec<&str> = added.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "x-amz-date",
            "x-amz-security-token",
            "x-amz-content-sha256",
            "authorization"
        ]
    );
    assert_eq!(added[0].1.value, "20261031T120000Z");
    assert_eq!(added[1].1.value, "SESSIONTOKEN");
    assert_eq!(added[2].1.value, BODY_SHA256);
    assert!(added[1].1.sensitive && added[3].1.sensitive && !added[0].1.sensitive);
    assert_eq!(
        authorization(&added),
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261031/us-west-2/bedrock/aws4_request, SignedHeaders={SIGNED}, Signature=25f7a3e720dbd591f5af0accafaf17b4a4a2f4499f6adc08a9a826c05ccd4098"
        )
    );
}

#[test]
fn a_mantle_request_signs_the_beta_query_and_omits_the_absent_token() {
    let signer = Signer::new(Credentials::new("AKIDEXAMPLE", SECRET, None).unwrap());
    let added = signer
        .sign(
            &Request {
                method: "POST",
                path: "/v1/messages",
                query: &[("beta", "true")],
                host: "bedrock-mantle.us-east-1.api.aws",
                headers: &CLAUDE_HEADERS,
                body: CLAUDE_BODY,
            },
            &Scope {
                region: "us-east-1",
                service: "bedrock-mantle",
            },
            CLAUDE_TIME,
        )
        .unwrap();
    assert!(added
        .iter()
        .all(|(name, _)| *name != "x-amz-security-token"));
    assert_eq!(
        authorization(&added),
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261031/us-east-1/bedrock-mantle/aws4_request, SignedHeaders={}, Signature=ff50f595b186b55ade44fcefee73ad86decd142380df3ed5ea31d7a3b25f1e30",
            SIGNED.replace(";x-amz-security-token", "")
        )
    );
}

#[test]
fn headers_added_after_signing_are_not_covered_and_unsignable_ones_are_dropped() {
    let signer = suite_signer();
    let sign = |headers: &[(&str, &str)]| {
        authorization(
            &signer
                .sign(
                    &Request {
                        method: "GET",
                        path: "/",
                        query: &[],
                        host: "example.amazonaws.com",
                        headers,
                        body: b"",
                    },
                    &SUITE_SCOPE,
                    SUITE_TIME,
                )
                .unwrap(),
        )
    };
    let plain = sign(&[]);
    // The SDK never signs these, wherever the client sets them.
    assert_eq!(
        sign(&[
            ("User-Agent", "x"),
            ("Connection", "keep-alive"),
            ("Authorization", "stale"),
            ("X-Amz-Date", "stale"),
            ("sec-fetch-mode", "cors"),
            ("Proxy-Connection", "keep-alive"),
            ("x-amzn-trace-id", "Root=1"),
        ]),
        plain
    );
    // A header the client sends is signed: it changes the signature and the list.
    let with_extra = sign(&[("x-client-request-id", "abc")]);
    assert_ne!(with_extra, plain);
    assert!(with_extra.contains("SignedHeaders=host;x-amz-date;x-client-request-id"));
}

#[test]
fn header_values_collapse_inner_whitespace_and_trim() {
    assert_eq!(normalize_value("  a \t b\n\n c  "), "a b c");
    assert_eq!(normalize_value("\u{feff}x\u{a0}y\u{3000}"), "x y");
    assert_eq!(normalize_value(""), "");
}

#[test]
fn paths_escape_once_more_and_keep_their_slashes() {
    assert_eq!(canonical_path("/"), "/");
    assert_eq!(canonical_path("/a//b/./c/../d/"), "/a/b/d/");
    assert_eq!(canonical_path("/a%2Fb:c"), "/a%252Fb%3Ac");
    assert_eq!(canonical_path("/it's(1)*!"), "/it%27s%281%29%2A%21");
}

#[test]
fn queries_sort_by_escaped_name_then_pair() {
    assert_eq!(
        canonical_query(&[("b", "2"), ("a-b", "x"), ("a", "1"), ("a", "0 1")]),
        "a=0%201&a=1&a-b=x&b=2"
    );
    assert_eq!(canonical_query(&[]), "");
}

#[test]
fn the_date_formats_across_leap_days_and_epoch_edges() {
    assert_eq!(amz_dates(0), ("19700101T000000Z".into(), "19700101".into()));
    assert_eq!(
        amz_dates(1_709_251_199),
        ("20240229T235959Z".into(), "20240229".into())
    );
    assert_eq!(
        amz_dates(1_709_251_200),
        ("20240301T000000Z".into(), "20240301".into())
    );
    assert_eq!(amz_dates(CLAUDE_TIME).0, "20261031T120000Z");
}

#[test]
fn the_signing_key_is_cached_per_day_region_and_service_and_bounded() {
    let signer = suite_signer();
    let scope = |region| Scope {
        region,
        service: "service",
    };
    let first = signer.signing_key("20150830", &scope("us-east-1")).unwrap();
    let again = signer.signing_key("20150830", &scope("us-east-1")).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    let other_region = signer.signing_key("20150830", &scope("eu-west-1")).unwrap();
    assert!(!Arc::ptr_eq(&first, &other_region));
    let other_day = signer.signing_key("20150831", &scope("us-east-1")).unwrap();
    assert_ne!(*first, *other_day);
    for day in 1..=KEY_CACHE_CAP + 2 {
        signer
            .signing_key(&format!("2016{day:04}"), &scope("us-east-1"))
            .unwrap();
    }
    assert!(signer.keys.len() <= KEY_CACHE_CAP);
}

#[test]
fn credentials_never_print_and_blank_ones_are_refused() {
    let credentials =
        Credentials::new("AKID", "super-secret-value", Some("session-secret")).unwrap();
    let printed = format!("{credentials:?} {:?}", Signer::new(credentials.clone()));
    assert!(!printed.contains("super-secret-value") && !printed.contains("session-secret"));
    assert!(printed.contains("AKID"));
    assert_eq!(
        Credentials::new(" ", "secret", None),
        Err(SigV4Error::EmptyCredentials)
    );
    assert_eq!(
        Credentials::new("AKID", "", None),
        Err(SigV4Error::EmptyCredentials)
    );
    let blank_token = Credentials::new("AKID", "secret", Some("  ")).unwrap();
    assert_eq!(blank_token.session_token, None);
}
