//! Typed invocation wire boundary. Admission and signature coverage require the
//! production owners to consume these inputs; serde acceptance grants no authority.
use super::*;
use gents_protocol::request_input::RequestInput;

#[test]
fn lean_request_inputs_decode_without_losing_explicit_issuance_facts() {
    let cases = &lean_contract_snapshot().request_input_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let input: RequestInput = serde_json::from_value(case["input"].clone())
            .unwrap_or_else(|error| panic!("{name}: typed input: {error}"));
        let encoded = serde_json::to_value(&input).unwrap();
        // The model emits nulls/empty lists; compact authoring omits them.
        // Every populated field must survive the production serializer exactly.
        for (field, value) in case["input"].as_object().unwrap() {
            if !value.is_null() && value.as_array().is_none_or(|xs| !xs.is_empty()) {
                if field == "queue" {
                    for (key, value) in value.as_object().unwrap() {
                        if !value.is_null() {
                            assert_eq!(&encoded[field][key], value, "{name}: queue.{key}");
                        }
                    }
                } else {
                    assert_eq!(&encoded[field], value, "{name}: {field}");
                }
            }
        }
        if let Some(goal) = input.goal_continuation {
            assert_eq!(
                encoded["goal_continuation"]["wrapup"], goal.wrapup,
                "{name}: false is an explicit original issuance fact"
            );
        }
    }
}

#[test]
fn request_input_rejects_legacy_controls_and_incomplete_goal_facts() {
    for value in [
        json!({"temperature": 0.7}),
        json!({"metadata": {"queue": {"source": "goal"}}}),
        json!({"skill_excludes": ["configured-skill"]}),
        json!({"goal_continuation": {"sequence": 1}}),
        json!({"goal_continuation": {"wrapup": false}}),
        json!({"goal_continuation": {"sequence": 1, "wrapup": false, "goal_id": "forged"}}),
    ] {
        assert!(
            serde_json::from_value::<RequestInput>(value.clone()).is_err(),
            "{value}"
        );
    }
    let compact = serde_json::to_value(RequestInput::default()).unwrap();
    assert_eq!(compact, json!({}));
}

/// The source-authentication observation belongs to existing admission/ACP.
/// This exercises its workspace tuple boundary, not a test-local resolver.
#[test]
fn lean_workspace_references_preserve_exact_owner_and_attenuate_authority() {
    fn lineage(value: &serde_json::Value) -> gents::lifecycle::WorkspaceLineage {
        let field = |name: &str| value[name].as_str().map(str::to_owned);
        gents::lifecycle::WorkspaceLineage {
            workspace_id: field("workspace_id"),
            workspace_owner_agent_did: field("workspace_owner_agent_did"),
            workspace_authority: field("workspace_authority"),
            workspace_seal_hash: field("workspace_seal_hash"),
        }
    }
    let cases = &lean_contract_snapshot().request_input_cases;
    assert!(cases
        .iter()
        .any(|case| case["name"] == "workspace-foreign-owner-same-label-denied"));
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let request = lineage(&case["workspace"]);
        let source = lineage(&case["workspace_source"]);
        assert_eq!(
            request
                .validate_source(
                    &source,
                    case["workspace_source_authenticated"].as_bool().unwrap(),
                )
                .is_ok(),
            case["expected_workspace_accepted"].as_bool().unwrap(),
            "{name}: actual workspace source owner"
        );
    }
}
