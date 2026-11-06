use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pack::AGENT_SLOT_REFERENCE_PREFIX;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectAgent {
    pub slot: String,
    pub agent_did: String,
}

pub(super) fn resolve_subject<'a>(
    subject: Option<&'a SubjectAgent>,
    slot: &str,
) -> Option<&'a str> {
    let subject = subject?;
    (!slot.is_empty() && subject.slot == slot && !subject.agent_did.is_empty())
        .then_some(subject.agent_did.as_str())
}

pub(crate) fn bind_subject_references(
    value: &mut Value,
    subject: Option<&SubjectAgent>,
) -> Result<()> {
    match value {
        Value::String(text) => {
            if let Some(slot) = text.strip_prefix(AGENT_SLOT_REFERENCE_PREFIX) {
                *text = resolve_subject(subject, slot)
                    .with_context(|| format!("eval subject agent slot {slot:?} is not bound"))?
                    .to_owned();
            }
        }
        Value::Array(values) => {
            for value in values {
                bind_subject_references(value, subject)?;
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                bind_subject_references(value, subject)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selected_subject_resolution_matches_lean() {
        for case in crate::lean_vocab_test::lean_pack_eval_subject_cases() {
            let selected = case.selected.as_ref().map(|binding| SubjectAgent {
                slot: binding.slot.clone(),
                agent_did: binding.agent_did.clone(),
            });
            assert_eq!(
                resolve_subject(selected.as_ref(), &case.slot),
                case.expected.as_deref(),
                "{}",
                case.name
            );
            let mut value =
                json!({"agent_did": {"_eq": format!("gents:agent-slot:{}", case.slot)}});
            let result = bind_subject_references(&mut value, selected.as_ref());
            assert_eq!(result.is_ok(), case.expected.is_some(), "{}", case.name);
            if let Some(expected) = &case.expected {
                assert_eq!(value["agent_did"]["_eq"], *expected, "{}", case.name);
            }
        }
    }
    #[test]
    fn grading_uses_the_installed_did_and_refuses_missing_and_foreign_slots() {
        use crate::eval::checks::CheckRegistry;
        use crate::eval::runner::executor::CaptureResult;
        use crate::eval::runner::scripted::ScriptedExecutor;
        use crate::eval::OutcomeKind;

        let registry = CheckRegistry::builtin();
        let mut stage =
            ScriptedExecutor::passed_evidence("did:test:node", "setup", "unused", vec![])
                .stages
                .remove(0);
        stage.subject_agent = Some(SubjectAgent {
            slot: "engineer".into(),
            agent_did: "did:key:actual-subject".into(),
        });
        stage.captures.insert(
            "tasks".into(),
            CaptureResult::Documents {
                rows: vec![json!({"task_id":"inbox", "agent_did":"did:key:actual-subject"})],
            },
        );
        let params = json!({"name":"tasks", "expect":[{"field":"agent_did", "equals":"gents:agent-slot:engineer"}]});
        let grade = |params: &Value, stage: &_| {
            registry
                .evaluate_bound("captured_fields_match", params, stage)
                .unwrap()
        };
        assert_eq!(grade(&params, &stage).kind, OutcomeKind::Passed);
        let mut decoy = stage.clone();
        let CaptureResult::Documents { rows } = decoy.captures.get_mut("tasks").unwrap() else {
            unreachable!()
        };
        rows[0]["agent_did"] = json!("did:key:another:engineer");
        assert_ne!(grade(&params, &decoy).kind, OutcomeKind::Passed);
        decoy.subject_agent = None;
        assert_eq!(grade(&params, &decoy).kind, OutcomeKind::Grader);
        let foreign = json!({"name":"tasks", "expect":[{"field":"agent_did", "equals":"gents:agent-slot:other"}]});
        assert_eq!(grade(&foreign, &stage).kind, OutcomeKind::Grader);
    }
}
