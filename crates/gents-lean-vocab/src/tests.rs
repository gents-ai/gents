use std::collections::BTreeSet;

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
fn decodes_pairing_reconcile_samples_with_stable_desired_resources() {
    let cases = lean_pairing_reconcile_cases();
    assert!(
        !cases.is_empty(),
        "Lean pairing_reconcile_cases must include multi-resource samples"
    );
    let names: BTreeSet<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    assert_eq!(
        names.len(),
        cases.len(),
        "pairing reconcile case names must be unique"
    );
    for case in cases {
        assert_eq!(
            case.before.desired_replicators, case.after.desired_replicators,
            "pairing case {} must keep the desired resource set stable",
            case.name
        );
        assert!(
            !case.before.desired_replicators.is_empty(),
            "pairing case {} must target at least one resource",
            case.name
        );
    }
}
