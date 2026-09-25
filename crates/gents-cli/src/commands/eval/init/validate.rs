//! A draft becomes a definition (assembly) and is held to the contract, the
//! check catalog and the subject (validation). Every failure is a message the
//! author can act on; they go back to it verbatim.

use std::collections::BTreeSet;

use gents::document_config::{EvalCapture, EvalDefinition, EvalSplit};
use gents::eval::checks::CheckRegistry;
use serde_json::{json, Value};

use super::dossier::Dossier;
use super::draft::Draft;

/// A draft the CLI completed into a definition: its id, owner, version and
/// subject are the CLI's, its cases the author's.
#[derive(Clone, Debug)]
pub(crate) struct Assembled {
    pub(crate) definition: EvalDefinition,
}

/// Minimums the interview settled.
pub(crate) struct Floors {
    pub(crate) validation_min: usize,
}

/// Step 2: the definition the draft describes. The author sets neither the
/// subject nor the comparability version; whatever it carried is ignored.
pub(crate) fn assemble(
    draft: &Draft,
    definition_id_flag: Option<&str>,
    owner: &str,
    slot: &str,
) -> Result<Assembled, Vec<String>> {
    let definition_id = definition_id_flag
        .map(str::to_owned)
        .or_else(|| draft.definition_id.clone())
        .ok_or_else(|| {
            vec!["the draft names no definition.definition_id and none was given with --definition-id".to_owned()]
        })?;
    let mut value = json!({
        "definition_id": definition_id,
        "agent_did": owner,
        "comparability_version": 1,
        "subject": {"kind": "behavior", "inference_slots": [slot]},
        "cases": draft.cases,
    });
    if let Some(title) = &draft.title {
        value["title"] = Value::String(title.clone());
    }
    serde_json::from_value(value)
        .map(|definition| Assembled { definition })
        .map_err(|error| vec![format!("the draft is not an eval definition: {error}")])
}

/// Steps 3 to 6, in order, stopping at the first step with messages.
pub(crate) fn validate(
    assembled: &Assembled,
    registry: &CheckRegistry,
    dossier: &Dossier,
    floors: &Floors,
) -> Result<(), Vec<String>> {
    let definition = &assembled.definition;
    definition
        .validate()
        .map_err(|error| vec![format!("{error:#}")])?;
    for step in [
        catalog_conformance(definition, registry),
        capture_conformance(definition, dossier),
        split_shape(definition, floors),
    ] {
        if !step.is_empty() {
            return Err(step.into_iter().collect());
        }
    }
    Ok(())
}

/// Step 4: every check is registered and its params satisfy its schema.
fn catalog_conformance(definition: &EvalDefinition, registry: &CheckRegistry) -> BTreeSet<String> {
    let mut messages = BTreeSet::new();
    for case in &definition.cases {
        for stage in &case.stages {
            let at = format!("case {} stage {}", case.case_id, stage.stage_id);
            for check in &stage.checks {
                let Some(registered) = registry.get(&check.check) else {
                    messages.insert(format!(
                        "{at}: unknown check {:?}; draft only checks from the catalog",
                        check.check
                    ));
                    continue;
                };
                let schema = registered.describe().params_schema;
                match jsonschema::validator_for(&schema) {
                    Ok(validator) => {
                        for error in validator.iter_errors(&check.params) {
                            messages.insert(format!("{at} check {} params: {error}", check.check));
                        }
                    }
                    Err(error) => {
                        messages.insert(format!(
                            "{at} check {}: its params schema does not compile: {error}",
                            check.check
                        ));
                    }
                }
            }
        }
    }
    messages
}

/// Step 5: documents captures name the subject's collections and their
/// fields; file captures stay inside the trial workspace.
fn capture_conformance(definition: &EvalDefinition, dossier: &Dossier) -> BTreeSet<String> {
    let mut messages = BTreeSet::new();
    for case in &definition.cases {
        for stage in &case.stages {
            for capture in &stage.capture {
                let at = format!(
                    "case {} stage {} capture {}",
                    case.case_id,
                    stage.stage_id,
                    capture.name()
                );
                match capture {
                    EvalCapture::Documents {
                        collection,
                        fields,
                        filter,
                        ..
                    } => match dossier.collections.get(collection) {
                        None => {
                            let known: Vec<&str> =
                                dossier.collections.keys().map(String::as_str).collect();
                            messages.insert(format!(
                                "{at}: collection {collection:?} is not one the subject shows (known: {})",
                                if known.is_empty() { "none".to_owned() } else { known.join(", ") }
                            ));
                        }
                        Some(known) => {
                            for field in fields.iter().filter(|field| !known.contains(*field)) {
                                messages.insert(format!(
                                    "{at}: collection {collection} has no field {field:?}"
                                ));
                            }
                            let mut keys = BTreeSet::new();
                            filter_fields(filter, &mut keys);
                            for key in keys.into_iter().filter(|key| !known.contains(*key)) {
                                messages.insert(format!(
                                    "{at}: filter key {key:?} is not a field of collection {collection}"
                                ));
                            }
                        }
                    },
                    EvalCapture::File { glob, .. } => {
                        if glob.starts_with('/')
                            || glob.starts_with('\\')
                            || glob.split(['/', '\\']).any(|part| part == "..")
                        {
                            messages.insert(format!(
                                "{at}: file glob {glob:?} must be relative to the trial workspace, without .."
                            ));
                        }
                    }
                }
            }
        }
    }
    messages
}

/// The field names a DefraDB filter object tests: its keys, through the
/// `_and`, `_or` and `_not` combinators. A field's own value holds operators
/// (`_eq`, `_in`, …), not fields, and `_docID` names the document itself.
fn filter_fields<'a>(filter: &'a Value, keys: &mut BTreeSet<&'a str>) {
    let Some(object) = filter.as_object() else {
        return;
    };
    for (key, value) in object {
        match key.as_str() {
            "_and" | "_or" => value
                .as_array()
                .into_iter()
                .flatten()
                .for_each(|filter| filter_fields(filter, keys)),
            "_not" => filter_fields(value, keys),
            "_docID" => {}
            field => {
                keys.insert(field);
            }
        }
    }
}

/// Step 6: every split is populated and validation meets its floor.
fn split_shape(definition: &EvalDefinition, floors: &Floors) -> BTreeSet<String> {
    let mut messages = BTreeSet::new();
    let count = |split: EvalSplit| {
        definition
            .cases
            .iter()
            .filter(|case| case.split == split)
            .count()
    };
    let missing: Vec<&str> = [
        (EvalSplit::Train, "train"),
        (EvalSplit::Validation, "validation"),
        (EvalSplit::HeldOut, "held_out"),
    ]
    .into_iter()
    .filter(|(split, _)| count(*split) == 0)
    .map(|(_, name)| name)
    .collect();
    if !missing.is_empty() {
        messages.insert(format!(
            "the draft has no {} cases; all three splits must be populated",
            missing.join(", ")
        ));
    }
    let validation = count(EvalSplit::Validation);
    if validation < floors.validation_min {
        messages.insert(format!(
            "the draft has {validation} validation cases; the floor is {}",
            floors.validation_min
        ));
    }
    let unique: BTreeSet<&str> = definition
        .cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect();
    if unique.len() != definition.cases.len() {
        messages.insert("the draft repeats a case_id".to_owned());
    }
    // A case's id names its sidecar file, so it is checked here, before
    // anything is written, and not left to the pack loader afterwards.
    for case_id in unique {
        if !gents::pack::is_valid_pack_name(&case_id.replace('-', "_")) {
            messages.insert(format!(
                "case_id {case_id:?} cannot name a case file: with `-` read as `_`, it must be a lowercase letter followed by lowercase letters, digits and underscores"
            ));
        }
    }
    messages
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::{json, Value};

    use super::*;

    pub(crate) const OWNER: &str = "did:key:owner";
    pub(crate) const SLOT: &str = "primary";

    /// The canary subject, with the collection its fixture schema installs at
    /// trial time.
    pub(crate) fn dossier() -> Dossier {
        Dossier {
            pack_name: "eval_canary".into(),
            pack_version: "1.0.0".into(),
            pack_digest: "sha256:canary".into(),
            behavior_id: "canary".into(),
            slot: SLOT.into(),
            collections: BTreeMap::from([(
                "CanaryItem".to_owned(),
                BTreeSet::from(["item_id".to_owned(), "label".to_owned()]),
            )]),
            text: String::new(),
        }
    }

    pub(crate) fn case(case_id: &str, split: &str) -> Value {
        json!({
            "case_id": case_id,
            "split": split,
            "stages": [{
                "stage_id": "answer",
                "prompt": "Record one item.",
                "deadline_secs": 600,
                "capture": [{
                    "kind": "documents",
                    "name": "items",
                    "collection": "CanaryItem",
                    "filter": {},
                    "fields": ["item_id", "label"],
                }],
                "checks": [{
                    "check": "captured_rows_count",
                    "params": {"name": "items", "min": 1},
                    "tier": "acceptance",
                }],
            }],
        })
    }

    /// One case per split: the smallest draft that validates at floor one.
    pub(crate) fn good() -> Draft {
        Draft {
            definition_id: Some("canary-quality".into()),
            title: Some("Canary quality".into()),
            cases: vec![
                case("train-a", "train"),
                case("val-a", "validation"),
                case("ho-a", "held_out"),
            ],
        }
    }

    const FLOOR_ONE: Floors = Floors { validation_min: 1 };

    fn check(draft: &Draft, floors: &Floors) -> Result<(), Vec<String>> {
        let assembled = assemble(draft, None, OWNER, SLOT)?;
        validate(&assembled, &CheckRegistry::builtin(), &dossier(), floors)
    }

    fn only_message(draft: &Draft, floors: &Floors) -> String {
        let messages = check(draft, floors).unwrap_err();
        assert_eq!(messages.len(), 1, "{messages:?}");
        messages.into_iter().next().unwrap()
    }

    fn first_stage(draft: &mut Draft) -> &mut Value {
        &mut draft.cases[0]["stages"][0]
    }

    #[test]
    fn a_good_draft_validates_and_assembly_owns_subject_and_version() {
        assert_eq!(check(&good(), &FLOOR_ONE), Ok(()));

        // What the author set for the subject or the version never lands.
        let reply = format!(
            "```json\n{}\n```",
            json!({
                "definition": {
                    "definition_id": "canary-quality",
                    "title": "Canary quality",
                    "comparability_version": 7,
                    "subject": {"kind": "behavior", "inference_slots": ["other"]},
                },
                "cases": good().cases,
            })
        );
        let draft = crate::commands::eval::init::draft::parse_reply(&reply)
            .unwrap()
            .unwrap();
        let assembled = assemble(&draft, Some("from-flag"), OWNER, SLOT).unwrap();
        let definition = &assembled.definition;
        assert_eq!(definition.definition_id, "from-flag");
        assert_eq!(definition.agent_did, OWNER);
        assert_eq!(definition.comparability_version, 1);
        assert_eq!(definition.subject.inference_slots, vec![SLOT.to_owned()]);
        assert_eq!(definition.title.as_deref(), Some("Canary quality"));
        assert_eq!(definition.cases.len(), 3);
    }

    #[test]
    fn a_missing_definition_id_is_refused() {
        let mut draft = good();
        draft.definition_id = None;
        let messages = assemble(&draft, None, OWNER, SLOT).unwrap_err();
        assert!(messages[0].contains("--definition-id"), "{messages:?}");
    }

    #[test]
    fn an_unknown_check_names_the_case_and_stage() {
        let mut draft = good();
        first_stage(&mut draft)["checks"][0]["check"] = json!("no_such");
        let message = only_message(&draft, &FLOOR_ONE);
        assert!(message.contains("unknown check \"no_such\""), "{message}");
        assert!(message.contains("case train-a stage answer"), "{message}");
    }

    #[test]
    fn params_that_violate_the_schema_are_refused() {
        let mut draft = good();
        first_stage(&mut draft)["checks"][0]["params"] = json!({"name": "items"});
        let message = only_message(&draft, &FLOOR_ONE);
        assert!(message.contains("params"), "{message}");
        assert!(message.contains("min"), "{message}");
        assert!(
            message.contains("case train-a stage answer check captured_rows_count"),
            "{message}"
        );
    }

    #[test]
    fn a_capture_outside_the_subject_is_refused() {
        let mut draft = good();
        first_stage(&mut draft)["capture"][0]["collection"] = json!("Mailbox");
        let message = only_message(&draft, &FLOOR_ONE);
        assert!(message.contains("Mailbox"), "{message}");

        let mut draft = good();
        first_stage(&mut draft)["capture"][0]["fields"] = json!(["item_id", "colour"]);
        let message = only_message(&draft, &FLOOR_ONE);
        assert!(message.contains("colour"), "{message}");
    }

    #[test]
    fn a_capture_filter_tests_only_fields_of_its_collection() {
        let mut draft = good();
        first_stage(&mut draft)["capture"][0]["filter"] = json!({
            "label": {"_eq": "one"},
            "_docID": {"_eq": "bae-1"},
            "_or": [{"item_id": {"_eq": "a"}}, {"_not": {"colour": {"_eq": "red"}}}],
        });
        let message = only_message(&draft, &FLOOR_ONE);
        assert!(
            message.contains("case train-a stage answer capture items"),
            "{message}"
        );
        assert!(message.contains("CanaryItem"), "{message}");
        assert!(message.contains("\"colour\""), "{message}");

        let mut draft = good();
        first_stage(&mut draft)["capture"][0]["filter"] =
            json!({"_and": [{"label": {"_eq": "one"}}, {"item_id": {"_in": ["a"]}}]});
        assert_eq!(check(&draft, &FLOOR_ONE), Ok(()));
    }

    #[test]
    fn a_file_capture_must_stay_in_the_workspace() {
        for glob in ["/etc/passwd", "../outside/*.txt"] {
            let mut draft = good();
            first_stage(&mut draft)["capture"] =
                json!([{"kind": "file", "name": "items", "glob": glob}]);
            let message = only_message(&draft, &FLOOR_ONE);
            assert!(message.contains(glob), "{message}");
        }
        let mut draft = good();
        first_stage(&mut draft)["capture"] =
            json!([{"kind": "file", "name": "items", "glob": "out/**/*.md"}]);
        assert_eq!(check(&draft, &FLOOR_ONE), Ok(()));
    }

    #[test]
    fn every_split_is_populated_and_validation_meets_its_floor() {
        let mut draft = good();
        draft.cases = vec![case("val-a", "validation")];
        let message = only_message(&draft, &FLOOR_ONE);
        assert!(message.contains("train, held_out"), "{message}");

        let mut draft = good();
        draft
            .cases
            .extend((0..4).map(|i| case(&format!("val-{i}"), "validation")));
        let message = only_message(&draft, &Floors { validation_min: 6 });
        assert!(message.contains("5 validation cases"), "{message}");
        assert!(message.contains('6'), "{message}");
    }

    #[test]
    fn a_case_id_that_cannot_name_a_case_file_is_refused() {
        for case_id in ["../../escape", "Train-A", "a/b", "train.a"] {
            let mut draft = good();
            draft.cases[0]["case_id"] = json!(case_id);
            let message = only_message(&draft, &FLOOR_ONE);
            assert!(
                message.contains(&format!("{case_id:?}")) && message.contains("case file"),
                "{message}"
            );
        }
    }

    #[test]
    fn the_definition_rules_run_before_catalog_captures_and_splits() {
        let mut draft = good();
        draft.cases[1]["case_id"] = json!("train-a");
        // Also an unknown check and a missing split: only step 3 answers.
        first_stage(&mut draft)["checks"][0]["check"] = json!("no_such");
        let message = only_message(&draft, &Floors { validation_min: 6 });
        assert!(message.contains("duplicate case_id train-a"), "{message}");
    }
}
