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
}
