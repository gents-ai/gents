use super::*;

fn base() -> Fields {
    Fields {
        version: "2.1.311".into(),
        fingerprint: "abc".into(),
        entrypoint: "cli".into(),
        provider: Provider::FirstParty,
        first_party_base_url: true,
        workload: None,
        subagent: false,
        previous_request_id: None,
        prompt_id: None,
        turn_origin: None,
        prompt_index: None,
        turn_index: None,
        gateway: None,
        project: None,
    }
}

#[test]
fn attribution_headers_match_lean() {
    gents_lean_vocab::families::claude_prompt_assembly_attribution::replay::<Provider>(|i| {
        let fields: Fields = gents_lean_vocab::family::recode(i);
        header(&fields)
    });
}

#[test]
fn fingerprint_known_answers() {
    let v = crate::profile::VERSION;
    assert_eq!(fingerprint("", v), "972");
    assert_eq!(fingerprint("hi", v), "972");
    assert_eq!(
        fingerprint("The quick brown fox jumps over the lazy dog", v),
        "3df"
    );
    assert_eq!(fingerprint("abcdefghijklmnopqrst", v), "3ae");
    assert_eq!(fingerprint("abcdefghijklmnopqrst", "9.9.9"), "49c");
    // units 4 and 7 are the high half of one emoji and the low half of another: re-paired
    assert_eq!(
        fingerprint("abcd\u{1F600}\u{1F680}efghijklmnopqrstu", v),
        "670"
    );
    // a high surrogate alone at unit 20 becomes U+FFFD
    assert_eq!(
        fingerprint(&format!("{}\u{1F600}", "a".repeat(20)), v),
        "653"
    );
    // a low surrogate alone at unit 4 becomes U+FFFD
    assert_eq!(fingerprint("abc\u{1F600}defghijklmnopqrstuvwxyz", v), "669");
    // non-BMP at 4, 7 and 20
    let mixed = format!("abcd\u{1F600}\u{1F680}{}\u{1F601}zz", "e".repeat(12));
    assert_eq!(fingerprint(&mixed, v), "fd4");
    assert_eq!(fingerprint(&mixed, "9.9.9"), "a15");
}

#[test]
fn field_validator_bounds() {
    assert!(valid_request_id(&format!("req_{}", "a".repeat(36))));
    assert!(valid_request_id("req_A-_9"));
    for bad in [
        format!("req_{}", "a".repeat(37)),
        "req_".into(),
        "req_a b".into(),
        "rex_abc".into(),
    ] {
        assert!(!valid_request_id(&bad), "{bad}");
    }

    for good in [
        "0123abcd-0123-abcd-0123-0123456789ab",
        "0123ABCD-0123-ABCD-0123-0123456789AB",
    ] {
        assert!(valid_uuid(good), "{good}");
    }
    for bad in [
        "0123abcd-0123-abcd-0123-0123456789a",
        "0123abcd-0123-abcd-0123-0123456789abc",
        "0123abcd-0123-abcd-0123-0123456789ab-",
        "0123abcg-0123-abcd-0123-0123456789ab",
    ] {
        assert!(!valid_uuid(bad), "{bad}");
    }

    assert!(valid_turn_origin(&format!("a{}", "_".repeat(31))));
    for bad in [
        format!("a{}", "_".repeat(32)),
        "".into(),
        "_a".into(),
        "aB".into(),
        "a1".into(),
    ] {
        assert!(!valid_turn_origin(&bad), "{bad}");
    }

    assert!(valid_index(0, 0) && valid_index(1, 10_000_000));
    assert!(!valid_index(1, 0) && !valid_index(0, 10_000_001));
}

#[test]
fn project_id_shape_and_sources() {
    let tail = "1".repeat(22);
    assert!(valid_project_id(&format!("claude_proj_01{tail}")));
    assert!(valid_project_id(&format!("claude_proj_staging_01{tail}")));
    assert!(valid_project_id(&format!("claude_proj_local_01{tail}")));
    assert!(!valid_project_id(&format!("claude_proj_02{tail}")));
    assert!(!valid_project_id(&format!(
        "claude_proj_01{}",
        "1".repeat(21)
    )));
    assert!(!valid_project_id(&format!(
        "claude_proj_01{}0",
        "1".repeat(21)
    )));
    assert!(!valid_project_id(&format!(
        "claude_proj_01{}l",
        "1".repeat(21)
    )));
    let mut fields = base();
    fields.project = Some(Project {
        id: format!("claude_proj_01{tail}"),
        source: "bogus".into(),
    });
    assert!(!header(&fields).contains("cc_project"));
    fields.project.as_mut().unwrap().source = "user".into();
    assert!(header(&fields).ends_with(&format!(
        "cc_project=claude_proj_01{tail}; cc_project_src=user;"
    )));
    fields.first_party_base_url = false;
    assert!(!header(&fields).contains("cc_project"));
}

fn jwt(claims: &str) -> String {
    use base64::Engine as _;
    let encode = |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
    format!("{}.{}.sig", encode("{\"alg\":\"none\"}"), encode(claims))
}

#[test]
fn gateway_mark_is_one_only_for_a_token_that_names_the_gateway_audience() {
    assert_eq!(gateway_mark(&jwt("{\"aud\":\"claude-gateway\"}")), "1");
    assert_eq!(
        gateway_mark(&jwt("{\"aud\":[\"other\",\"claude-gateway\"]}")),
        "1"
    );
    assert_eq!(gateway_mark(&jwt("{\"aud\":\"other\"}")), "env");
    assert_eq!(gateway_mark(&jwt("{}")), "env");
    assert_eq!(gateway_mark("not-a-jwt"), "env");
    assert_eq!(gateway_mark("a.!!!.c"), "env");
}
