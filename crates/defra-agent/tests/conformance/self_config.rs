//! SelfConfig conformance home: pin the production patch layer
//! (`defra_agent::config_client::patch`) against the Lean `SelfConfig` model.
//!
//! Two fences:
//! - the per-target field tables (all/writable/protected, unique key,
//!   category) must equal the Lean contract tables, and the Lean `all_fields`
//!   must equal the bundled SDL field lists — a schema field added without a
//!   self-config classification fails here;
//! - the generated patch-merge witness cases must replay identically through
//!   the production merge (identity immutability, field containment,
//!   admissibility, transactional accept/reject shape, no-lockout gate).

use std::collections::BTreeMap;

use defra_agent::config_client::patch::{
    apply_patch, ensure_admissible, SelfConfigPatch, SelfConfigTarget, ALL_SELF_CONFIG_TARGETS,
    DEFAULT_SELF_CONFIG_CATEGORIES, SELF_CONFIG_CATEGORIES,
};
use serde_json::{Map, Value};

use crate::lean_vocab_test::{
    lean_self_config_cases, lean_self_config_field_tables, LeanSelfConfigCase,
};

/// Field names of a bundled SDL type declaration, in declaration order.
fn sdl_field_names(sdl: &str) -> Vec<String> {
    sdl.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with("type ")
                || line.starts_with('}')
            {
                return None;
            }
            line.split(':').next().map(|name| name.trim().to_string())
        })
        .collect()
}

fn bundled_sdl(collection: &str) -> &'static str {
    match collection {
        "AgentBehavior" => defra_agent_protocol::schemas::AGENT_BEHAVIOR,
        "ToolSelection" => defra_agent_protocol::schemas::TOOL_SELECTION,
        "InferenceProfile" => defra_agent_protocol::schemas::INFERENCE_PROFILE,
        "InferenceBackend" => defra_agent_protocol::schemas::INFERENCE_BACKEND,
        "ToolServiceRegistry" => defra_agent_protocol::schemas::TOOL_SERVICE_REGISTRY,
        "Task" => defra_agent_protocol::schemas::TASK,
        "Schedule" => defra_agent_protocol::schemas::SCHEDULE,
        "EventTrigger" => defra_agent_protocol::schemas::EVENT_TRIGGER,
        other => panic!("no bundled SDL mapping for {other}"),
    }
}

pub(super) fn self_config_field_tables_match_lean_contract() {
    let tables = lean_self_config_field_tables();
    assert_eq!(
        tables.len(),
        ALL_SELF_CONFIG_TARGETS.len(),
        "Lean must emit one field table per self-config target"
    );

    for table in tables {
        let target = SelfConfigTarget::from_collection_name(&table.collection)
            .unwrap_or_else(|| panic!("unknown Lean self-config collection {}", table.collection));
        assert_eq!(
            table.unique_field,
            target.unique_field(),
            "{}: unique field diverged",
            table.collection
        );
        assert_eq!(
            table.category,
            target.category(),
            "{}: category diverged",
            table.collection
        );
        assert_eq!(
            table.all_fields,
            target.all_fields().to_vec(),
            "{}: all_fields diverged from Lean",
            table.collection
        );
        assert_eq!(
            table.writable_fields,
            target.writable_fields().to_vec(),
            "{}: writable_fields diverged from Lean",
            table.collection
        );
        assert_eq!(
            table.protected_fields,
            target.protected_fields(),
            "{}: protected_fields diverged from Lean",
            table.collection
        );

        let sdl_fields = sdl_field_names(bundled_sdl(&table.collection));
        assert_eq!(
            table.all_fields, sdl_fields,
            "{}: Lean all_fields diverged from the bundled SDL — classify the new/removed \
             schema field in Proofs/SelfConfig/Types.lean and config_client::patch",
            table.collection
        );
    }

    let lean_categories: Vec<&str> = tables.iter().map(|table| table.category.as_str()).collect();
    for category in &lean_categories {
        assert!(
            SELF_CONFIG_CATEGORIES.contains(category),
            "category {category} missing from SELF_CONFIG_CATEGORIES"
        );
    }
    for category in DEFAULT_SELF_CONFIG_CATEGORIES {
        assert!(
            SELF_CONFIG_CATEGORIES.contains(&category),
            "default category {category} missing from SELF_CONFIG_CATEGORIES"
        );
    }
}

fn doc_map(entries: &[crate::lean_vocab_test::LeanSelfConfigFieldValue]) -> Map<String, Value> {
    entries
        .iter()
        .map(|entry| (entry.field.clone(), Value::String(entry.value.clone())))
        .collect()
}

fn case_patch(case: &LeanSelfConfigCase) -> SelfConfigPatch {
    case.patch
        .iter()
        .map(|entry| {
            let value = match entry.action.as_str() {
                "set" => Some(Value::String(
                    entry
                        .value
                        .clone()
                        .expect("set patch entry must carry a value"),
                )),
                "clear" => None,
                other => panic!("unknown patch action {other}"),
            };
            (entry.field.clone(), value)
        })
        .collect()
}

pub(super) fn generated_self_config_cases_fence_patch_merge() {
    let cases = lean_self_config_cases();
    assert!(!cases.is_empty(), "no self-config cases emitted by Lean");

    for case in cases {
        let target = SelfConfigTarget::from_collection_name(&case.collection)
            .unwrap_or_else(|| panic!("case {}: unknown collection", case.name));
        let stored = doc_map(&case.doc);
        let patch = case_patch(case);

        let admissible = ensure_admissible(target, &patch).is_ok();
        assert_eq!(
            admissible, case.admissible,
            "case {}: admissibility diverged from Lean",
            case.name
        );

        let merged = apply_patch(target, &stored, &patch);

        // Mirror of the Lean `step`: validation is an oracle flag; the
        // no-lockout guard observes the merged gate field.
        let guard_ok = !case.guarded
            || merged.get("enable_self_config") == Some(&Value::String("true".to_string()));
        let accepted = admissible && case.validates && guard_ok;
        assert_eq!(
            accepted, case.accepted,
            "case {}: accept/reject outcome diverged from Lean",
            case.name
        );

        let result = if accepted { &merged } else { &stored };
        let expected: BTreeMap<&str, &Value> = case
            .result
            .iter()
            .map(|entry| (entry.field.as_str(), &entry.value))
            .map(|(field, value)| {
                (
                    field,
                    result
                        .get(field)
                        .filter(|actual| actual.as_str() == Some(value))
                        .unwrap_or_else(|| {
                            panic!(
                                "case {}: field {field} expected {value:?}, got {:?}",
                                case.name,
                                result.get(field)
                            )
                        }),
                )
            })
            .collect();
        assert_eq!(
            expected.len(),
            result.len(),
            "case {}: result carries fields the Lean projection does not",
            case.name
        );

        let protected_preserved = target
            .protected_fields()
            .iter()
            .all(|field| result.get(*field) == stored.get(*field));
        assert_eq!(
            protected_preserved, case.protected_preserved,
            "case {}: identity-immutability witness diverged",
            case.name
        );

        let containment = target.all_fields().iter().all(|field| {
            merged.get(*field) == stored.get(*field)
                || (target.is_writable(field) && patch.iter().any(|(patched, _)| patched == field))
        });
        assert_eq!(
            containment, case.containment_holds,
            "case {}: field-containment witness diverged",
            case.name
        );

        let unchanged_on_reject = accepted || *result == stored;
        assert_eq!(
            unchanged_on_reject, case.unchanged_on_reject,
            "case {}: transactional-totality witness diverged",
            case.name
        );

        let gate_on_after_accept = !(case.guarded && accepted)
            || result.get("enable_self_config") == Some(&Value::String("true".to_string()));
        assert_eq!(
            gate_on_after_accept, case.gate_on_after_accept,
            "case {}: no-lockout witness diverged",
            case.name
        );
    }
}
