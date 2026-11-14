use super::*;

#[test]
fn reports_malformed_arm() {
    let lean = r#"
namespace Sample

def toDefraDB : Sample -> String
  | .one "one"

end Sample
"#;

    let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
    assert!(matches!(
        error,
        LeanVocabularyParseError::MalformedArm {
            reason: "missing `=>`",
            ..
        }
    ));
}

#[test]
fn lean_executable_contracts_cover_initial_domains() {
    let sweep_ids = &lean_contract_snapshot().recovery_sweep_ids;
    assert!(!sweep_ids.is_empty(), "the model registers recovery sweeps");
    for sweep_id in sweep_ids {
        assert!(
            lean_recovery_sweep_cases()
                .iter()
                .any(|case| &case.sweep_id == sweep_id),
            "registered recovery sweep {sweep_id} has no emitted case"
        );
    }
    for case in lean_recovery_sweep_cases() {
        assert!(
            sweep_ids.contains(&case.sweep_id),
            "recovery sweep case {} names an unregistered sweep {}",
            case.name,
            case.sweep_id
        );
    }
}
