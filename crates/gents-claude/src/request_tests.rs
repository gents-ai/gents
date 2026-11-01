use super::*;
use crate::headers::{request_headers, Auth, HeaderContext, RequestClass};

fn triples<'a>(h: &'a [(&'a str, &'a str)]) -> Vec<(&'a str, &'a str, bool)> {
    h.iter().map(|&(n, v)| (n, v, false)).collect()
}

fn names(head: &RequestHead) -> Vec<&str> {
    head.headers().map(|(n, _)| n).collect()
}

const MAIN: &[(&str, &str)] = &[
    ("Accept", "application/json"),
    ("X-Stainless-Retry-Count", "0"),
    ("X-Stainless-Timeout", "600"),
    ("X-Stainless-Lang", "js"),
    ("X-Stainless-Package-Version", "0.128.0"),
    ("X-Stainless-OS", "Linux"),
    ("X-Stainless-Arch", "x64"),
    ("X-Stainless-Runtime", "node"),
    ("X-Stainless-Runtime-Version", "v26.12.1"),
    ("anthropic-dangerous-direct-browser-access", "true"),
    ("anthropic-version", "2023-06-01"),
    ("Authorization", "Bearer t"),
    ("x-app", "cli"),
    ("User-Agent", "claude-cli/2.1.311 (external, cli)"),
    ("X-Claude-Code-Session-Id", "s"),
    ("x-claude-code-request-class", "main"),
    ("content-type", "application/json"),
    ("anthropic-beta", "a,b"),
    ("x-client-request-id", "u"),
];

#[test]
fn main_thread_head_bytes() {
    let head = RequestHead::new(
        "POST",
        "/v1/messages?beta=true",
        "api.anthropic.com",
        triples(MAIN),
        true,
    );
    let expected = "POST /v1/messages?beta=true HTTP/1.1\r\n\
Accept: application/json\r\n\
Authorization: Bearer t\r\n\
Content-Type: application/json\r\n\
User-Agent: claude-cli/2.1.311 (external, cli)\r\n\
X-Claude-Code-Session-Id: s\r\n\
X-Stainless-Arch: x64\r\n\
X-Stainless-Lang: js\r\n\
X-Stainless-OS: Linux\r\n\
X-Stainless-Package-Version: 0.128.0\r\n\
X-Stainless-Retry-Count: 0\r\n\
X-Stainless-Runtime: node\r\n\
X-Stainless-Runtime-Version: v26.12.1\r\n\
X-Stainless-Timeout: 600\r\n\
anthropic-beta: a,b\r\n\
anthropic-dangerous-direct-browser-access: true\r\n\
anthropic-version: 2023-06-01\r\n\
x-app: cli\r\n\
x-claude-code-request-class: main\r\n\
x-client-request-id: u\r\n\
Content-Encoding: gzip\r\n\
Connection: keep-alive\r\n\
Host: api.anthropic.com\r\n\
Accept-Encoding: gzip, deflate, br, zstd\r\n\
Content-Length: 1234\r\n\r\n";
    assert_eq!(head.encode(1234), expected.as_bytes());
}

#[test]
fn generated_headers_sort_end_to_end() {
    let ctx = HeaderContext {
        entrypoint: "cli",
        agent_sdk_version: None,
        client_app: None,
        workload: None,
        background: false,
        session_id: "s",
        os: "Linux",
        arch: "x64",
        auth: Auth::Bearer("t"),
        hint_headers: true,
        client_request_id_header: true,
        workspace_id: None,
        agent: None,
        agent_type: None,
        request_class: RequestClass::Main,
        prompt_id: None,
        betas: &["a", "b"],
        client_request_id: "u",
        retry_count: 0,
        timeout_ms: 600_000,
    };
    let generated = request_headers(&ctx);
    let head = RequestHead::new(
        "POST",
        "/v1/messages?beta=true",
        "api.anthropic.com",
        generated
            .iter()
            .map(|(n, v)| (*n, v.value.as_str(), v.sensitive)),
        true,
    );
    assert_eq!(
        names(&head),
        [
            "Accept",
            "Authorization",
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
            "x-claude-code-request-class",
            "x-client-request-id",
            "Content-Encoding",
            "Connection",
            "Host",
            "Accept-Encoding",
        ]
    );
    let debug = format!("{head:?}");
    assert!(debug.contains("Authorization: <redacted>"));
    assert!(!debug.contains("Bearer t"));
}

#[test]
fn gzip_off_omits_content_encoding() {
    let head = RequestHead::new("POST", "/", "h", triples(MAIN), false);
    assert!(!names(&head).contains(&"Content-Encoding"));
}

#[test]
fn user_headers_suppress_defaults() {
    let head = RequestHead::new(
        "POST",
        "/",
        "h",
        [
            ("connection", "close", false),
            ("accept", "text/plain", false),
            ("user-agent", "ua", false),
            ("host", "other", false),
            ("accept-encoding", "identity", false),
            ("content-encoding", "br", false),
            ("Content-Length", "9", false),
            ("transfer-encoding", "chunked", false),
        ],
        true,
    );
    let out = String::from_utf8(head.encode(5)).unwrap();
    assert_eq!(
        out,
        "POST / HTTP/1.1\r\nAccept: text/plain\r\nAccept-Encoding: identity\r\nConnection: close\r\n\
Content-Encoding: br\r\nHost: other\r\nUser-Agent: ua\r\nContent-Length: 5\r\n\r\n"
    );
}

#[test]
fn duplicate_names_keep_first_spelling_last_value() {
    let head = RequestHead::new(
        "GET",
        "/",
        "h",
        [
            ("X-Foo", "1", false),
            ("x-foo", "2", false),
            ("ACCEPT", "a", false),
            ("accept", "b", false),
        ],
        false,
    );
    let got: Vec<_> = head.headers().collect();
    assert_eq!(&got[..2], &[("Accept", "b"), ("X-Foo", "2")]);
}

#[test]
fn values_are_trimmed() {
    let head = RequestHead::new("GET", "/", "h", [("X-A", " \t\r\nv w \n", false)], false);
    assert_eq!(head.headers().next(), Some(("X-A", "v w")));
}

#[test]
fn wire_name_table() {
    assert_eq!(wire_name("content-type"), "Content-Type");
    assert_eq!(wire_name("ACCEPT"), "Accept");
    assert_eq!(wire_name("etag"), "ETag");
    assert_eq!(wire_name("x-api-key"), "x-api-key");
    assert_eq!(wire_name("X-Api-Key"), "X-Api-Key");
    assert_eq!(COMMON_HEADER_NAMES.len(), 93);
}

#[test]
fn padding_bounds_and_alphabet() {
    assert_eq!(padding_len(0.0), 0);
    assert_eq!(padding_len(0.999_999_999), 256);
    let mut body = b"{}".to_vec();
    let mut flip = false;
    pad_body(&mut body, 4, || {
        flip = !flip;
        flip
    });
    assert_eq!(body, b"{}\n \t \t");
    let mut empty = Vec::new();
    pad_body(&mut empty, 0, || true);
    assert_eq!(empty, b"\n");
}

#[test]
fn utf16_len_counts_units() {
    assert_eq!(utf16_len("abc"), 3);
    assert_eq!(utf16_len("\u{1F600}"), 2);
    assert_eq!(utf16_len("\u{e9}"), 1);
}

#[test]
fn gzip_decision_boundaries() {
    let host = "api.anthropic.com";
    assert!(!gzip_body(host, 4095, false, false));
    assert!(gzip_body(host, 4096, false, false));
    assert!(!gzip_body("example.com", 5000, false, false));
    assert!(!gzip_body(host, 5000, true, false));
    assert!(!gzip_body(host, 5000, false, true));
}

#[test]
fn gzip_rejection_table() {
    let m = |status, id, cf, msg| gzip_rejected(status, id, cf, msg);
    for msg in [
        "The request body is not valid JSON",
        "Failed to parse request: could not parse request body as JSON",
        "bad json: invalid character 'x' looking for beginning of value",
        "bad json: unexpected EOF",
        "bad json: unexpected end of JSON input",
        "bad json: EOF",
    ] {
        assert!(m(400, Some("req_1"), false, msg), "{msg}");
        assert!(m(400, None, true, msg), "{msg}");
        assert!(!m(400, Some("x_1"), false, msg), "{msg}");
        assert!(!m(400, None, false, msg), "{msg}");
        assert!(!m(500, Some("req_1"), false, msg), "{msg}");
    }
    assert!(!m(400, Some("req_1"), false, "bad json: unexpected EOF!"));
    assert!(!m(400, Some("req_1"), false, "bad json: invalid character"));
    assert!(!m(400, Some("req_1"), false, "bad json: other"));
    assert!(!m(400, Some("req_1"), false, "invalid request"));
}
