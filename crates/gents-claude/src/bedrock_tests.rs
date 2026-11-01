//! Bedrock rule tests: path escaping, the body rewrite and its key order, the
//! region-prefix rewrite table, profile selection, the profile list page and
//! the mapping of event-stream frames to Messages events.

use super::*;
use crate::eventstream::HeaderValue;

#[test]
fn the_invoke_path_escapes_like_the_sdk_template() {
    let path = |model: &str| invoke_path(model).unwrap();
    assert_eq!(
        path("us.anthropic.claude-opus-4-5-20251101-v1:0"),
        "/model/us.anthropic.claude-opus-4-5-20251101-v1:0/invoke-with-response-stream"
    );
    // `:` stays, `/` becomes %2F, and the characters the template keeps stay.
    assert_eq!(
        path("arn:aws:bedrock:us-west-2:123456789012:application-inference-profile/abc"),
        "/model/arn:aws:bedrock:us-west-2:123456789012:application-inference-profile%2Fabc/invoke-with-response-stream"
    );
    assert_eq!(
        path("a b%c"),
        "/model/a%20b%25c/invoke-with-response-stream"
    );
    assert_eq!(
        path("it's(1)*!$&+,;=@~_"),
        "/model/it's(1)*!$&+,;=@~_/invoke-with-response-stream"
    );
    assert_eq!(path("é"), "/model/%C3%A9/invoke-with-response-stream");
    for bad in ["", ".", ".."] {
        assert!(
            matches!(invoke_path(bad), Err(BedrockError::ModelId(_))),
            "{bad:?}"
        );
    }
    // The check runs on the escaped segment: a literal `%2e` escapes to `%252e`.
    assert!(invoke_path("...").is_ok() && invoke_path("%2e").is_ok());
}

const PLANNED: &str = r#"{"model":"m","messages":[],"system":[],"tools":[],"metadata":{"user_id":"u"},"max_tokens":8,"thinking":{"type":"adaptive"},"context_management":{"edits":[]},"output_config":{"effort":"high"},"diagnostics":{"a":1},"stream":true}"#;

fn transformed(body: &str, body_betas: &[&str], header_betas: &[&str]) -> String {
    String::from_utf8(transform_body(body.as_bytes(), body_betas, header_betas).unwrap()).unwrap()
}

#[test]
fn the_body_drops_model_and_stream_and_appends_the_version() {
    assert_eq!(
        transformed(PLANNED, &[], &[]),
        r#"{"messages":[],"system":[],"tools":[],"metadata":{"user_id":"u"},"max_tokens":8,"thinking":{"type":"adaptive"},"context_management":{"edits":[]},"output_config":{"effort":"high"},"diagnostics":{"a":1},"anthropic_version":"bedrock-2023-05-31"}"#
    );
}

#[test]
fn body_betas_sit_before_output_config_and_the_header_betas_are_not_copied() {
    let out = transformed(
        PLANNED,
        &[
            "interleaved-thinking-2025-05-14",
            "thinking-token-count-2026-05-13",
        ],
        &["claude-code-20250219"],
    );
    assert_eq!(
        out,
        r#"{"messages":[],"system":[],"tools":[],"metadata":{"user_id":"u"},"max_tokens":8,"thinking":{"type":"adaptive"},"context_management":{"edits":[]},"anthropic_beta":["interleaved-thinking-2025-05-14","thinking-token-count-2026-05-13"],"output_config":{"effort":"high"},"diagnostics":{"a":1},"anthropic_version":"bedrock-2023-05-31"}"#
    );
}

#[test]
fn without_body_betas_the_header_betas_follow_the_version() {
    let out = transformed(PLANNED, &[], &["claude-code-20250219", "effort-2025-11-24"]);
    assert!(out.ends_with(
        r#""anthropic_version":"bedrock-2023-05-31","anthropic_beta":["claude-code-20250219","effort-2025-11-24"]}"#
    ), "{out}");
    // No header betas and no body betas: no array at all.
    assert!(!transformed(PLANNED, &[], &[]).contains("anthropic_beta"));
}

#[test]
fn body_betas_land_at_the_end_when_no_later_key_exists() {
    let out = transformed(
        r#"{"model":"m","messages":[],"stream":true}"#,
        &["context-1m-2025-08-07"],
        &[],
    );
    assert_eq!(
        out,
        r#"{"messages":[],"anthropic_beta":["context-1m-2025-08-07"],"anthropic_version":"bedrock-2023-05-31"}"#
    );
}

#[test]
fn an_existing_version_is_kept_and_a_non_object_body_fails() {
    let out = transformed(
        r#"{"model":"m","anthropic_version":"x","stream":true}"#,
        &[],
        &[],
    );
    assert_eq!(out, r#"{"anthropic_version":"x"}"#);
    assert!(transform_body(b"[1]", &[], &[]).is_err());
    assert!(matches!(
        transform_body(&[0xff, 0xfe], &[], &[]),
        Err(BedrockError::BodyNotUtf8)
    ));
}

#[test]
fn the_region_prefix_follows_the_region_family() {
    for (region, prefix) in [
        ("us-east-1", "us"),
        ("us-west-2", "us"),
        ("us-gov-west-1", "us-gov"),
        ("eu-west-1", "eu"),
        ("eu-central-1", "eu"),
        ("ap-southeast-2", "apac"),
        ("ap-northeast-1", "apac"),
        ("ca-central-1", "global"),
        ("sa-east-1", "global"),
        ("me-south-1", "global"),
    ] {
        assert_eq!(region_prefix(region), prefix, "{region}");
    }
}

#[test]
fn model_ids_take_the_region_prefix_by_the_sdk_rule() {
    let table = [
        (
            "us.anthropic.claude-opus-4-5-20251101-v1:0",
            "eu",
            "eu.anthropic.claude-opus-4-5-20251101-v1:0",
        ),
        (
            "us.anthropic.claude-opus-4-5-20251101-v1:0",
            "us",
            "us.anthropic.claude-opus-4-5-20251101-v1:0",
        ),
        (
            "us.anthropic.claude-sonnet-5",
            "apac",
            "apac.anthropic.claude-sonnet-5",
        ),
        (
            "us.anthropic.claude-sonnet-5",
            "global",
            "global.anthropic.claude-sonnet-5",
        ),
        (
            "us.anthropic.claude-sonnet-5",
            "us-gov",
            "us-gov.anthropic.claude-sonnet-5",
        ),
        (
            "jp.anthropic.claude-sonnet-5",
            "eu",
            "eu.anthropic.claude-sonnet-5",
        ),
        (
            "au.anthropic.claude-sonnet-5",
            "us",
            "us.anthropic.claude-sonnet-5",
        ),
        (
            "us-gov.anthropic.claude-sonnet-5",
            "us",
            "us.anthropic.claude-sonnet-5",
        ),
        (
            "global.anthropic.claude-sonnet-5",
            "eu",
            "eu.anthropic.claude-sonnet-5",
        ),
        (
            "anthropic.claude-haiku-4-5",
            "eu",
            "eu.anthropic.claude-haiku-4-5",
        ),
        ("custom-model-id", "eu", "custom-model-id"),
        ("amazon.nova-pro-v1:0", "eu", "amazon.nova-pro-v1:0"),
    ];
    for (id, prefix, want) in table {
        assert_eq!(rewrite_model_id(id, prefix), want, "{id} -> {prefix}");
    }
    // An ARN of a system profile rewrites inside the ARN.
    assert_eq!(
        rewrite_model_id(
            "arn:aws:bedrock:us-east-1:123:inference-profile/us.anthropic.claude-sonnet-5",
            "eu"
        ),
        "arn:aws:bedrock:us-east-1:123:inference-profile/eu.anthropic.claude-sonnet-5"
    );
    // An application profile ARN is not a geo profile and is sent as given.
    let application = "arn:aws:bedrock:us-west-2:123:application-inference-profile/abc";
    assert_eq!(rewrite_model_id(application, "eu"), application);
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn a_listed_profile_is_chosen_by_prefix_then_by_name() {
    let profiles = ids(&[
        "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "us.anthropic.claude-sonnet-4-20250514-v1:0",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "apac.anthropic.claude-opus-5",
    ]);
    // claude-sonnet-4 must not match claude-sonnet-4-5.
    assert_eq!(
        select_profile(&profiles, "claude-sonnet-4", "us"),
        Some("us.anthropic.claude-sonnet-4-20250514-v1:0")
    );
    assert_eq!(
        select_profile(&profiles, "claude-sonnet-4-5-20250929", "us"),
        Some("us.anthropic.claude-sonnet-4-5-20250929-v1:0")
    );
    assert_eq!(
        select_profile(&profiles, "claude-sonnet-4-5-20250929", "eu"),
        Some("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
    );
    // No profile with the wanted prefix: any profile that names the model.
    assert_eq!(
        select_profile(&profiles, "claude-opus-5", "us"),
        Some("apac.anthropic.claude-opus-5")
    );
    assert_eq!(select_profile(&profiles, "claude-haiku-9", "us"), None);
    assert_eq!(select_profile(&[], "claude-opus-5", "us"), None);
}

#[test]
fn a_model_name_followed_by_a_version_is_a_different_model() {
    for (profile, first_party, want) in [
        (
            "us.anthropic.claude-sonnet-4-5-v1",
            "claude-sonnet-4",
            false,
        ),
        (
            "us.anthropic.claude-sonnet-4-20250514-v1:0",
            "claude-sonnet-4",
            true,
        ),
        ("us.anthropic.claude-sonnet-4", "claude-sonnet-4", true),
        ("us.anthropic.claude-sonnet-45", "claude-sonnet-4", false),
        ("us.anthropic.claude-sonnet-4-123", "claude-sonnet-4", true),
        ("a.claude-x-1 us.claude-x", "claude-x", true),
    ] {
        assert_eq!(names_model(profile, first_party), want, "{profile}");
    }
}

#[test]
fn a_profile_page_keeps_anthropic_ids_and_the_next_token() {
    let page = parse_profile_page(
        br#"{"inferenceProfileSummaries":[{"inferenceProfileId":"us.anthropic.claude-sonnet-5"},{"inferenceProfileId":"us.amazon.nova-pro-v1:0"},{"inferenceProfileName":"x"}],"nextToken":"t2"}"#,
    )
    .unwrap();
    assert_eq!(page.ids, ["us.anthropic.claude-sonnet-5"]);
    assert_eq!(page.next_token.as_deref(), Some("t2"));
    let last = parse_profile_page(br#"{"inferenceProfileSummaries":[],"nextToken":""}"#).unwrap();
    assert_eq!(
        last,
        ProfilePage {
            ids: vec![],
            next_token: None
        }
    );
    assert_eq!(parse_profile_page(b"{}").unwrap().ids.len(), 0);
    assert!(matches!(
        parse_profile_page(b"nope"),
        Err(BedrockError::ProfileList(_))
    ));
}

#[test]
fn only_an_own_non_govcloud_runtime_host_is_the_default_endpoint() {
    for (endpoint, want) in [
        ("https://bedrock-runtime.us-west-2.amazonaws.com", true),
        ("https://bedrock-runtime.eu-central-1.amazonaws.com/", true),
        ("https://bedrock-runtime-fips.us-east-1.amazonaws.com", true),
        ("https://bedrock-runtime.ap-southeast-2.api.aws", true),
        (
            "https://vpce-0abc-def.bedrock-runtime.us-east-1.vpce.amazonaws.com",
            true,
        ),
        ("https://bedrock-runtime.us-gov-west-1.amazonaws.com", false),
        ("http://bedrock-runtime.us-west-2.amazonaws.com", false),
        ("https://proxy.example.com", false),
        (
            "https://bedrock-runtime.us-west-2.amazonaws.com.evil.example",
            false,
        ),
        ("http://127.0.0.1:8080", false),
        ("not a url", false),
    ] {
        assert_eq!(is_own_runtime_endpoint(endpoint), want, "{endpoint}");
    }
}

fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Frame {
    Frame {
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), HeaderValue::String((*value).to_owned())))
            .collect(),
        payload: payload.to_vec(),
    }
}

fn chunk(json: &str) -> Frame {
    let encoded = base64::engine::general_purpose::STANDARD.encode(json);
    frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        format!(r#"{{"bytes":"{encoded}"}}"#).as_bytes(),
    )
}

#[test]
fn a_chunk_becomes_the_messages_event_of_its_type() {
    assert_eq!(
        map_frame(&chunk(r#"{"type":"message_stop"}"#)).unwrap(),
        Mapped::Event("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".into())
    );
    let delta =
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"h\né"}}"#;
    let Mapped::Event(text) = map_frame(&chunk(delta)).unwrap() else {
        panic!("event");
    };
    assert!(text.starts_with("event: content_block_delta\ndata: "));
    // One data line even when the text has a newline: it is escaped in the JSON.
    assert_eq!(text.lines().count(), 3, "{text}");
    let data = text.lines().nth(1).unwrap().strip_prefix("data: ").unwrap();
    let back: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(back["delta"]["text"], "h\né");
    // Re-emitted as `JSON.stringify(JSON.parse(..))`: key order kept, not sorted.
    assert_eq!(data, delta);
    let spaced = r#"{ "type" : "ping" , "z" : 1 , "a" : [ 1 , 2 ] }"#;
    assert_eq!(
        map_frame(&chunk(spaced)).unwrap(),
        Mapped::Event("event: ping\ndata: {\"type\":\"ping\",\"z\":1,\"a\":[1,2]}\n\n".into())
    );
}

#[test]
fn chunks_without_a_type_or_bytes_and_other_events_are_ignored() {
    assert_eq!(map_frame(&chunk(r#"{"x":1}"#)).unwrap(), Mapped::Ignored);
    assert_eq!(map_frame(&chunk(r#"{"type":3}"#)).unwrap(), Mapped::Ignored);
    assert_eq!(map_frame(&chunk("[1]")).unwrap(), Mapped::Ignored);
    let empty = frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        br#"{"bytes":""}"#,
    );
    assert_eq!(map_frame(&empty).unwrap(), Mapped::Ignored);
    let other = frame(
        &[(":message-type", "event"), (":event-type", "metadata")],
        b"{}",
    );
    assert_eq!(map_frame(&other).unwrap(), Mapped::Ignored);
}

#[test]
fn a_chunk_that_is_not_json_fails_loud() {
    let not_json = frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        b"nope",
    );
    assert!(matches!(map_frame(&not_json), Err(StreamError::Chunk(_))));
    let bad_base64 = frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        br#"{"bytes":"!!"}"#,
    );
    assert!(matches!(map_frame(&bad_base64), Err(StreamError::Chunk(_))));
    assert!(matches!(
        map_frame(&chunk("{broken")),
        Err(StreamError::Chunk(_))
    ));
}

fn error_data(mapped: Mapped) -> serde_json::Value {
    let Mapped::Error(text) = mapped else {
        panic!("error event");
    };
    assert!(text.starts_with("event: error\ndata: "), "{text}");
    serde_json::from_str(text.lines().nth(1).unwrap().strip_prefix("data: ").unwrap()).unwrap()
}

#[test]
fn a_modeled_exception_becomes_a_typed_error_event() {
    for name in MODELED_EXCEPTIONS {
        let exception = frame(
            &[(":message-type", "exception"), (":exception-type", name)],
            br#"{"message":"slow down"}"#,
        );
        let data = error_data(map_frame(&exception).unwrap());
        assert_eq!(data["type"], "error");
        assert_eq!(data["error"]["type"], name);
        assert_eq!(data["error"]["message"], "slow down");
    }
    let no_message = frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "throttlingException"),
        ],
        b"{}",
    );
    assert_eq!(
        error_data(map_frame(&no_message).unwrap())["error"]["message"],
        ""
    );
    let bad = frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "throttlingException"),
        ],
        b"nope",
    );
    assert!(matches!(map_frame(&bad), Err(StreamError::Exception(_))));
}

#[test]
fn an_unmodeled_exception_and_a_named_error_keep_their_names() {
    let unknown = frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "SurpriseException"),
        ],
        b"plain text",
    );
    let data = error_data(map_frame(&unknown).unwrap());
    assert_eq!(data["error"]["type"], "SurpriseException");
    assert_eq!(data["error"]["message"], "plain text");

    let named = frame(
        &[
            (":message-type", "error"),
            (":error-code", "InternalFailure"),
            (":error-message", "boom"),
        ],
        b"",
    );
    let data = error_data(map_frame(&named).unwrap());
    assert_eq!(data["error"]["type"], "InternalFailure");
    assert_eq!(data["error"]["message"], "boom");
    let blank = frame(&[(":message-type", "error"), (":error-code", "X")], b"");
    assert_eq!(
        error_data(map_frame(&blank).unwrap())["error"]["message"],
        "UnknownError"
    );
}

#[test]
fn an_unrecognized_frame_kind_fails_loud() {
    for headers in [
        vec![(":message-type", "weird")],
        vec![],
        vec![(":message-type", "error")],
        vec![(":message-type", "exception")],
    ] {
        assert!(matches!(
            map_frame(&frame(&headers, b"")),
            Err(StreamError::Unrecognized(_))
        ));
    }
}
