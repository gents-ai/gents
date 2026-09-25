//! What the CLI tells the author. The authoring contract itself is the
//! `eval_author` context's system prompt; the first turn carries only what
//! changes per init: the subject dossier, the check catalog and the floors.

use std::fmt::Write as _;

use gents::eval::checks::CheckDescription;

use super::dossier::Dossier;
use super::validate::Floors;

/// The first line of every turn that returns a failed draft to the author.
pub(crate) const VALIDATION_PREFIX: &str =
    "The draft did not validate; revise and reply with a new draft.";

/// Drafts that may fail validation before the interview gives up.
pub(crate) const MAX_VALIDATION_ROUNDS: usize = 3;

/// The author's first turn: `# Subject` (the dossier), `# Check catalog`
/// (every check the registry ships, as JSON), and the floors the operator
/// set.
pub(crate) fn first_turn(
    dossier: &Dossier,
    catalog: &[CheckDescription],
    floors: &Floors,
) -> String {
    let mut text = dossier.text.trim_end().to_owned();
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
    use crate::commands::eval::init::validate::tests::dossier;

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

    /// The authoring contract the `eval_author` pack ships.
    const AUTHOR_PROMPT: &str = include_str!(
        "../../../../../../packs/eval_author/agent_behaviors/eval_author/system_prompt.md"
    );

    /// The first fenced json block after `## The case shape`: the worked
    /// example.
    fn worked_example(prompt: &str) -> &str {
        let at = prompt
            .find("## The case shape")
            .expect("a case shape section");
        prompt[at..]
            .split("```json\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .expect("a json block after the case shape heading")
    }

    /// Every way the example strays from the registry: an unregistered
    /// check, or params its schema refuses.
    fn example_violations(example: &str, registry: &CheckRegistry) -> Vec<String> {
        let case: gents::document_config::EvalCase =
            serde_json::from_str(example).expect("the worked example is an EvalCase");
        let mut violations = Vec::new();
        for stage in &case.stages {
            for check in &stage.checks {
                let Some(registered) = registry.get(&check.check) else {
                    violations.push(format!("unknown check {}", check.check));
                    continue;
                };
                let schema = registered.describe().params_schema;
                let validator = jsonschema::validator_for(&schema).expect("a compiling schema");
                violations.extend(
                    validator
                        .iter_errors(&check.params)
                        .map(|error| format!("{}: {error}", check.check)),
                );
            }
        }
        violations
    }

    #[test]
    fn the_worked_example_names_only_registered_checks_with_valid_params() {
        let violations =
            example_violations(worked_example(AUTHOR_PROMPT), &CheckRegistry::builtin());
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn an_example_that_invents_a_check_is_caught() {
        let example = worked_example(AUTHOR_PROMPT);
        let registry = CheckRegistry::builtin();
        let first = registry.names()[0];
        let invented = example.replace(first, "invented_check");
        let violations = example_violations(&invented, &registry);
        assert!(
            violations.iter().any(|v| v.contains("invented_check")),
            "{violations:?}"
        );
    }

    #[test]
    fn the_contract_names_the_floor_flag_and_the_reducers_scoring_reduces() {
        assert!(
            AUTHOR_PROMPT.contains("--validation-min"),
            "{AUTHOR_PROMPT}"
        );
        assert!(!AUTHOR_PROMPT.contains("guaranteed"), "{AUTHOR_PROMPT}");
        for reducer in ["`all`", "`weighted_mean`", "`last_stage`"] {
            assert!(AUTHOR_PROMPT.contains(reducer), "{reducer}");
        }
        assert!(AUTHOR_PROMPT.contains("acceptance"), "{AUTHOR_PROMPT}");
    }
}
