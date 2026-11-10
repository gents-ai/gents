use super::*;

fn provider(name: &str) -> Provider {
    match name {
        "firstParty" => Provider::FirstParty,
        "vertex" => Provider::Vertex,
        "bedrock" => Provider::Bedrock,
        "anthropicAws" => Provider::AnthropicAws,
        "anthropicGoogleCloud" => Provider::AnthropicGoogleCloud,
        "foundry" => Provider::Foundry,
        "mantle" => Provider::Mantle,
        "gateway" => Provider::Gateway,
        other => panic!("unknown provider {other}"),
    }
}

fn text(case: &serde_json::Value, key: &str) -> Option<String> {
    case[key].as_str().map(str::to_owned)
}

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
fn generated_attribution_cases_render_the_header() {
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["prompt_assembly_claude_attribution_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let project = match (text(case, "project_id"), text(case, "project_source")) {
            (Some(id), Some(source)) => Some(Project { id, source }),
            _ => None,
        };
        let fields = Fields {
            version: text(case, "version").unwrap(),
            fingerprint: text(case, "fingerprint").unwrap(),
            entrypoint: text(case, "entrypoint").unwrap(),
            provider: provider(case["provider"].as_str().unwrap()),
            first_party_base_url: case["first_party_base_url"].as_bool().unwrap(),
            workload: text(case, "workload"),
            subagent: case["subagent"].as_bool().unwrap(),
            previous_request_id: text(case, "previous_request_id"),
            prompt_id: text(case, "prompt_id"),
            turn_origin: text(case, "turn_origin"),
            prompt_index: case["prompt_index"].as_u64(),
            turn_index: case["turn_index"].as_u64(),
            gateway: text(case, "gateway"),
            project,
        };
        assert_eq!(
            header(&fields),
            case["header"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn header_orders_the_slots() {
    let mut fields = base();
    fields.workload = Some("cron".into());
    fields.subagent = true;
    fields.previous_request_id = Some("req_abc".into());
    fields.prompt_index = Some(0);
    fields.turn_index = Some(1);
    fields.gateway = Some("1".into());
    assert_eq!(
        header(&fields),
        "x-anthropic-billing-header: cc_version=2.1.311.abc; cc_entrypoint=cli; cch=00000; \
         cc_workload=cron; cc_is_subagent=true; cc_prev_req=req_abc; \
         cc_prompt_index=0; cc_turn_index=1; cc_gateway=1;"
    );
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
fn request_id_bounds() {
    assert!(valid_request_id(&format!("req_{}", "a".repeat(36))));
    assert!(!valid_request_id(&format!("req_{}", "a".repeat(37))));
    assert!(!valid_request_id("req_"));
    assert!(valid_request_id("req_A-_9"));
    assert!(!valid_request_id("req_a b"));
    assert!(!valid_request_id("rex_abc"));
}

#[test]
fn uuid_is_case_insensitive_and_exact() {
    assert!(valid_uuid("0123abcd-0123-abcd-0123-0123456789ab"));
    assert!(valid_uuid("0123ABCD-0123-ABCD-0123-0123456789AB"));
    assert!(!valid_uuid("0123abcd-0123-abcd-0123-0123456789a"));
    assert!(!valid_uuid("0123abcd-0123-abcd-0123-0123456789abc"));
    assert!(!valid_uuid("0123abcd-0123-abcd-0123-0123456789ab-"));
    assert!(!valid_uuid("0123abcg-0123-abcd-0123-0123456789ab"));
}

#[test]
fn turn_origin_bounds() {
    assert!(valid_turn_origin(&format!("a{}", "_".repeat(31))));
    assert!(!valid_turn_origin(&format!("a{}", "_".repeat(32))));
    assert!(!valid_turn_origin(""));
    assert!(!valid_turn_origin("_a"));
    assert!(!valid_turn_origin("aB"));
    assert!(!valid_turn_origin("a1"));
}

#[test]
fn index_bounds() {
    assert!(valid_index(0, 0));
    assert!(!valid_index(1, 0));
    assert!(valid_index(1, 10_000_000));
    assert!(!valid_index(0, 10_000_001));
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

#[test]
fn every_betas_provider_maps_to_its_attribution_provider() {
    use crate::betas::Provider as Betas;
    for (betas, expected) in [
        (Betas::FirstParty, Provider::FirstParty),
        (Betas::Vertex, Provider::Vertex),
        (Betas::Bedrock, Provider::Bedrock),
        (Betas::AnthropicAws, Provider::AnthropicAws),
        (Betas::AnthropicGoogleCloud, Provider::AnthropicGoogleCloud),
        (Betas::Foundry, Provider::Foundry),
        (Betas::Mantle, Provider::Mantle),
        (Betas::Gateway, Provider::Gateway),
    ] {
        assert_eq!(Provider::from(betas), expected);
    }
}
