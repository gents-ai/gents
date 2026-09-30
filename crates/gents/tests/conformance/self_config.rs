//! Generated field tables and patch results checked against the existing patch owner.
//! Guarded rows replay the Lean guard verdict through the production guard of
//! their target (Tools and Behavior no-lockout, Backend auth). Reference
//! validation and transactional rejection need an end-to-end ConfigApplyTxn
//! consumer; this test does not simulate them.
use crate::lean_vocab_test::{
    lean_self_config_cases, lean_self_config_field_tables, LeanSelfConfigCase,
};
use gents::config_client::patch::{
    apply_patch, ensure_admissible, SelfConfigPatch, SelfConfigTarget, ALL_SELF_CONFIG_TARGETS,
};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

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

pub(super) fn self_config_field_tables_match_lean_contract() {
    let tables = lean_self_config_field_tables();
    let actual: BTreeSet<_> = ALL_SELF_CONFIG_TARGETS
        .iter()
        .map(|t| t.collection_name())
        .collect();
    let expected: BTreeSet<_> = tables.iter().map(|t| t.collection.as_str()).collect();
    assert_eq!(actual, expected, "runtime self-config target inventory");
    for table in tables {
        let target =
            SelfConfigTarget::from_collection_name(&table.collection).expect("runtime target");
        assert_eq!(
            table.unique_field,
            target.unique_field(),
            "{}",
            table.collection
        );
        assert_eq!(table.category, target.category(), "{}", table.collection);
        assert_eq!(
            table.all_fields,
            target.all_fields(),
            "{}",
            table.collection
        );
        assert_eq!(
            table.writable_fields,
            target.writable_fields(),
            "{}",
            table.collection
        );
        assert_eq!(
            table.protected_fields,
            target.protected_fields(),
            "{}",
            table.collection
        );
    }
}

pub(super) fn generated_self_config_cases_fence_patch_merge() {
    let cases = lean_self_config_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let target =
            SelfConfigTarget::from_collection_name(&case.collection).expect("runtime target");
        let stored = doc_map(&case.doc);
        let patch = case_patch(case);
        assert_eq!(
            ensure_admissible(target, &patch).is_ok(),
            case.admissible,
            "{}: runtime patch admissibility",
            case.name
        );
        // Accepted rows expose the merge result. Rejected rows expose stored
        // state, so comparing a locally selected fallback would prove nothing
        // about the real transaction or reference/no-lockout validator.
        if case.accepted {
            assert_eq!(
                apply_patch(target, &stored, &patch),
                doc_map(&case.result),
                "{}: runtime patch merge",
                case.name
            );
        }
        if case.guarded && case.admissible && case.validates {
            let stored = typed_doc(target, &case.doc);
            let patch = patch
                .into_iter()
                .map(|(field, value)| (field, value.map(parse_nested)))
                .collect();
            let candidate = apply_patch(target, &stored, &patch);
            let verdict = match target {
                SelfConfigTarget::Tools => {
                    gents::self_config::guard_tools_keep_control(&stored, &candidate)
                }
                SelfConfigTarget::AgentBehavior => {
                    gents::self_config::guard_behavior_keeps_reach(&stored, &candidate)
                }
                SelfConfigTarget::InferenceBackend => {
                    gents::self_config::guard_backend_auth(&stored, &candidate)
                }
                SelfConfigTarget::InferenceProfile => {
                    let backend = |doc: &Map<String, Value>| {
                        let id = doc.get("backend_id")?;
                        case.backends
                            .iter()
                            .find(|backend| &parse_nested(backend.backend_id.clone().into()) == id)
                            .map(typed_backend)
                    };
                    match backend(&candidate) {
                        Some(next) => gents::self_config::guard_backend_choice(
                            backend(&stored).as_ref(),
                            &next,
                        ),
                        None => Err(anyhow::anyhow!("{}: unknown next backend", case.name)),
                    }
                }
                other => panic!("{}: no runtime guard for {other:?}", case.name),
            };
            assert_eq!(
                verdict.is_ok(),
                case.accepted,
                "{}: runtime guard",
                case.name
            );
        }
    }
}

fn typed_backend(
    backend: &crate::lean_vocab_test::LeanSelfConfigBackend,
) -> gents::InferenceBackend {
    let id = parse_nested(backend.backend_id.clone().into());
    serde_json::from_value(serde_json::json!({
        "agent_did": "did:key:agent-a",
        "backend_id": id,
        "name": id,
        "provider_kind": backend.provider_kind,
        "endpoint": "http://127.0.0.1:1/v1",
        "auth": parse_nested(Value::String(backend.auth.clone())),
    }))
    .unwrap_or_else(|error| panic!("{}: {error}", backend.backend_id))
}

/// Lean rows abstract nested Tools groups as their canonical JSON text.
fn parse_nested(value: Value) -> Value {
    let text = value.as_str().expect("Lean group value is text");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn typed_doc(
    target: SelfConfigTarget,
    entries: &[crate::lean_vocab_test::LeanSelfConfigFieldValue],
) -> Map<String, Value> {
    let mut doc: Map<String, Value> = entries
        .iter()
        .map(|entry| {
            let value = Value::String(entry.value.clone());
            let value = if entry.field == target.unique_field() {
                value
            } else {
                parse_nested(value)
            };
            (entry.field.clone(), value)
        })
        .collect();
    doc.entry(target.unique_field())
        .or_insert_with(|| Value::String("doc-1".into()));
    doc.insert("agent_did".into(), Value::String("did:key:agent-a".into()));
    doc
}
