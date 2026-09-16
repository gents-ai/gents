use serde_json::Value;

#[test]
fn setup_guidance_covers_conversation_authority_and_repeatability() {
    let prompt = gents_protocol::SETUP_STEWARD_PROMPT;
    for required in [
        "understand what the user uses AI for",
        "Ask permission before inspecting",
        "no real discovery capability is available",
        "explicitly labelled test-input inventory",
        "source attribution and a confidence label",
        "minimal useful proposal",
        "Never create duplicate behavior, profile, skill, or backend rows",
        "Server default changes require approved intent",
        "Start a new session on the working behavior",
    ] {
        assert!(
            prompt.contains(required),
            "missing Setup guidance: {required}"
        );
    }
}

#[test]
fn setup_inventory_fixture_is_unambiguously_synthetic_input() {
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/setup/existing-config-inventory.json"
    ))
    .expect("valid setup fixture JSON");
    assert_eq!(fixture["fixture_kind"], "test_input");
    assert!(fixture["warning"]
        .as_str()
        .is_some_and(|warning| warning.contains("not a scan")));
    assert_eq!(fixture["conflicts"].as_array().unwrap().len(), 1);
    assert!(fixture["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["state"] == "disabled"));
    assert!(fixture.get("credentials").is_none());
}
