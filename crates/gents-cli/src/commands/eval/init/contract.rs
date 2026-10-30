use std::fmt::Write as _;

use gents::eval::checks::CheckDescription;

use super::dossier::Dossier;
use super::validate::Floors;

pub(crate) const VALIDATION_PREFIX: &str =
    "The draft did not validate; revise and reply with a new draft.";

pub(crate) const MAX_VALIDATION_ROUNDS: usize = 3;

pub(crate) fn first_turn(
    dossier: &Dossier,
    catalog: &[CheckDescription],
    floors: &Floors,
) -> String {
    let mut text = dossier.text.trim_end().to_owned();
    text.push_str("\n\n# Runtime captures\n\nRead-only documents captures may also name these runtime collections: ");
    text.push_str(
        &super::dossier::runtime_collections()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
    );
    text.push_str(". Invalid-field errors list the available fields. Seeds must use the subject's collections.\n");
    text.push_str("\n\n# Check catalog\n\n```json\n");
    text.push_str(
        &serde_json::to_string_pretty(catalog).unwrap_or_else(|error| {
            tracing::error!(error = %error, "the check catalog does not serialize");
            "[]".to_owned()
        }),
    );
    text.push_str("\n```\n\n# Floors\n\n");
    let _ = writeln!(
        text,
        "- the validation split needs at least {} cases",
        floors.validation_min
    );
    text
}

#[cfg(test)]
mod tests {
    use gents::eval::checks::CheckRegistry;

    use super::*;
    use crate::commands::eval::init::validate::{case_catalog_conformance, tests::dossier};

    #[test]
    fn the_first_turn_is_the_dossier_the_catalog_and_the_floors() {
        let mut subject = dossier();
        subject.text = "# Subject\n\n## Identity\n\n- pack: eval_canary 1.0.0\n".into();
        let catalog = CheckRegistry::builtin().catalog();
        let turn = first_turn(&subject, &catalog, &Floors { validation_min: 6 });
        assert!(turn.starts_with("# Subject\n"), "{turn}");
        let at = turn.find("# Check catalog").expect("a catalog section");
        let body = &turn[at..];
        let json = body
            .split("```json\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .unwrap();
        let parsed: Vec<CheckDescription> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, catalog);
        assert!(turn.contains("at least 6 cases"), "{turn}");
        assert!(!turn.contains("## How to reply with a draft"), "{turn}");
    }

    const EXAMPLE: &str = r#"{
        "case_id": "disk-warning",
        "split": "validation",
        "stages": [{
            "stage_id": "check",
            "prompt": "Run the monitor.",
            "checks": [{"check": "captured_rows_count", "params": {"name": "findings", "min": 1}, "tier": "acceptance"}]
        }]
    }"#;

    #[test]
    fn an_example_that_invents_a_check_is_caught() {
        let registry = CheckRegistry::builtin();
        let valid: gents::document_config::EvalCase = serde_json::from_str(EXAMPLE).unwrap();
        assert!(case_catalog_conformance(&valid, &registry).is_empty());
        let invented: gents::document_config::EvalCase =
            serde_json::from_str(&EXAMPLE.replace("captured_rows_count", "invented_check"))
                .unwrap();
        let violations = case_catalog_conformance(&invented, &registry);
        assert!(
            violations.iter().any(|v| v.contains("invented_check")),
            "{violations:?}"
        );
    }
}
