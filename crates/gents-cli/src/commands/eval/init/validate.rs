//! A draft becomes a definition (assembly) and is held to the contract, the
//! check catalog and the subject (validation). Every failure is a message the
//! author can act on; they go back to it verbatim.

use std::collections::BTreeSet;

use gents::document_config::{EvalCapture, EvalCase, EvalDefinition, EvalSplit};
use gents::eval::checks::CheckRegistry;
use serde_json::{json, Value};

use super::dossier::Dossier;
use super::draft::Draft;

#[derive(Clone, Debug)]
pub(crate) struct Assembled {
    pub(crate) definition: EvalDefinition,
}

pub(crate) struct Floors {
    pub(crate) validation_min: usize,
}

/// The definition the draft describes: the CLI owns the subject and the
/// comparability version, whatever the draft carried is ignored.
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
        "node_did": owner,
        "comparability_version": 1,
        "subject": {"kind": "agent", "inference_slots": [slot]},
        "cases": draft.cases,
    });
    if let Some(title) = &draft.title {
        value["title"] = Value::String(title.clone());
    }
    serde_json::from_value(value)
        .map(|definition| Assembled { definition })
        .map_err(|error| vec![format!("the draft is not an eval definition: {error}")])
}

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
        definition
            .cases
            .iter()
            .flat_map(|case| case_catalog_conformance(case, registry))
            .collect(),
        capture_conformance(definition, dossier),
        split_shape(definition, floors),
    ] {
        if !step.is_empty() {
            return Err(step.into_iter().collect());
        }
    }
    Ok(())
}

/// The single owner of the rule, so `gents eval checks --validate-case`
/// (a pack's own contract tests) and the author's draft validation cannot
/// disagree on what the catalog admits.
pub(crate) fn case_catalog_conformance(
    case: &EvalCase,
    registry: &CheckRegistry,
) -> BTreeSet<String> {
    let mut messages = BTreeSet::new();
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
    messages
}

fn capture_conformance(definition: &EvalDefinition, dossier: &Dossier) -> BTreeSet<String> {
    let mut messages = BTreeSet::new();
    for case in &definition.cases {
        for stage in &case.stages {
            if let Some(seed) = &stage.seed {
                if !dossier.collections.contains_key(&seed.collection) {
                    messages.insert(format!(
                        "case {} stage {} seed: collection {:?} is not one the subject shows (known: {})",
                        case.case_id,
                        stage.stage_id,
                        seed.collection,
                        known_collections(dossier)
                    ));
                }
            }
            for capture in &stage.capture {
                let at = format!(
                    "case {} stage {} capture {}",
                    case.case_id,
                    stage.stage_id,
                    capture.name()
                );
                match capture {
                    // A stage may create the collections whose schemas it captures.
                    EvalCapture::Schema { .. } => {}
                    EvalCapture::Documents {
                        collection,
                        fields,
                        filter,
                        ..
                    } => match dossier.collections.get(collection) {
                        None => {
                            messages.insert(format!(
                                "{at}: collection {collection:?} is not one the subject shows (known: {})",
                                known_collections(dossier)
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

fn known_collections(dossier: &Dossier) -> String {
    let known: Vec<&str> = dossier.collections.keys().map(String::as_str).collect();
    if known.is_empty() {
        "none".to_owned()
    } else {
        known.join(", ")
    }
}

/// The field names a DefraDB filter object tests: its keys, through the
/// `_and`, `_or` and `_not` combinators; `_docID` names the document itself.
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

