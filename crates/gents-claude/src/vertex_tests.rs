use super::*;

#[test]
fn the_stream_path_names_project_region_and_model() {
    assert_eq!(
        stream_path("my-project", "us-east5", "claude-opus-4-5@20251101"),
        "/projects/my-project/locations/us-east5/publishers/anthropic/models/claude-opus-4-5@20251101:streamRawPredict"
    );
    // The pathname setter escapes only its percent-encode set: `@` and `:` stay.
    assert_eq!(
        stream_path("p", "global", "a b?#{}"),
        "/projects/p/locations/global/publishers/anthropic/models/a%20b%3F%23%7B%7D:streamRawPredict"
    );
}

fn transformed(body: &str) -> String {
    String::from_utf8(transform_body(body.as_bytes()).unwrap()).unwrap()
}

#[test]
fn the_body_drops_model_keeps_stream_and_appends_the_version() {
    assert_eq!(
        transformed(r#"{"model":"claude-sonnet-5","messages":[],"max_tokens":8,"stream":true}"#),
        r#"{"messages":[],"max_tokens":8,"stream":true,"anthropic_version":"vertex-2023-10-16"}"#
    );
}

#[test]
fn an_existing_version_is_kept_and_a_non_object_body_fails() {
    assert_eq!(
        transformed(r#"{"anthropic_version":"x","model":"m","stream":true}"#),
        r#"{"anthropic_version":"x","stream":true}"#
    );
    assert!(transform_body(b"[1]").is_err());
    assert!(transform_body(&[0xff]).is_err());
}
