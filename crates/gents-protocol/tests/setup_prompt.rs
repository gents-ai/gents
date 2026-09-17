use serde_json::Value;

#[test]
fn setup_guidance_covers_conversation_authority_and_repeatability() {
    let prompt = gents_protocol::SETUP_STEWARD_PROMPT;
    for required in [
        "understand what the user uses AI for",
        "ask for explicit approval of that source/root tuple",
        "config discovery scan --source SOURCE_ID KIND SCOPE PATH",
        "structured `mapping` with `level` and `reasons`",
        "After discovery, stop",
        "A disabled item stays disabled",
        "Apply only after the user approves that preview",
        "If real discovery is unavailable",
        "explicitly labelled test-input inventory",
        "source attribution and a confidence label",
        "minimal useful proposal",
        "Never create duplicate behavior, profile, skill, or backend rows",
        "Server default changes require approved intent",
        "Start a new session on the working behavior",
        "config behavior context edit",
        "A prompt edit does not require cloning",
        "Titles are not deduplication keys",
        "not a custom MailItem collection",
        "scheduled request's requester may differ",
        "Mailbox attention is not authority",
        "Keep the database configuration minimal",
        "identify its exact ID from your write receipt",
        "verify both removal and preservation",
        "canonical_mailbox_entries",
        "Deduplication is configured policy, not a prompt convention",
        "The runtime owns notification identity, routing and provenance",
        "Prefer structured config inputs",
        "native JSON patch values in `set`",
        "created/reused/updated receipt",
        "Ordinary event tasks should leave goal_objective_template unset",
    ] {
        assert!(
            prompt.contains(required),
            "missing Setup guidance: {required}"
        );
    }
}

#[test]
fn setup_inventory_fixture_matches_the_production_discovery_shape() {
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/setup/existing-config-inventory.json"
    ))
    .expect("valid setup fixture JSON");
    let keys = fixture
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        [
            "conflicts",
            "items",
            "schema_version",
            "sources",
            "truncation",
            "trust_notice",
            "warnings"
        ]
        .into_iter()
        .collect()
    );
    assert!(fixture["sources"].as_array().unwrap().iter().all(|source| {
        source.get("outcome").is_some()
            && source.get("scan_outcome").is_none()
            && source["selected_root"]
                .as_str()
                .is_some_and(|root| root.starts_with("/synthetic-fixture/"))
    }));
    assert!(fixture["items"].as_array().unwrap().iter().all(|item| {
        item["mapping"]["level"].is_string()
            && item["mapping"]["reasons"].is_array()
            && item.get("mapping_support").is_none()
    }));
    assert_eq!(fixture["conflicts"].as_array().unwrap().len(), 1);
    assert!(fixture["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["state"] == "disabled"));
    assert!(fixture.get("credentials").is_none());
}

#[test]
fn setup_discovery_flow_orders_consent_clarification_apply_and_verification() {
    let prompt = gents_protocol::SETUP_STEWARD_PROMPT;
    assert!(!prompt.contains("config discover scan"));
    let position = |text: &str| {
        prompt
            .find(text)
            .unwrap_or_else(|| panic!("missing {text:?}"))
    };
    assert!(position("ask for explicit approval") < position("config discovery scan"));
    assert!(
        position("After discovery, stop") < position("Propose only the smallest configuration")
    );
    assert!(position("Preview every write") < position("Apply only after the user approves"));
    assert!(
        position("Apply only after the user approves")
            < position("verify with targeted config reads")
    );
    assert!(
        position("A disabled item stays disabled") < position("Apply only after the user approves")
    );
    assert!(prompt.contains("Credentials/OAuth remain operator-owned"));
}
