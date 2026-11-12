use super::*;

use gents_protocol::request_input::RequestInput;

#[test]
fn lean_workspace_references_preserve_exact_owner_and_attenuate_authority() {
    fn lineage(value: &serde_json::Value) -> gents_session::request::lifecycle::WorkspaceLineage {
        let field = |name: &str| value[name].as_str().map(str::to_owned);
        gents_session::request::lifecycle::WorkspaceLineage {
            workspace_id: field("workspace_id"),
            workspace_owner_node_did: field("workspace_owner_node_did"),
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

#[test]
