//! The target: the one field of the one document an optimization job may
//! change, and the closure that field sits in.
//!
//! Nothing here consults a proposer or a model. A proposed text arrives as a
//! value and this module turns it into a patch deterministically, which is why
//! `Proposer` never needs a `ConfigAccess`: the only writer of a patch is here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config_client::{
    desired_state_document_digest, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan, DesiredStateExpectation,
};
use crate::Collection;

/// The one field an optimization job may change in v1 (ruling R2). A Task's
/// `prompt_template` is deferred; adding it is a new variant and a new
/// structural check, not a flag on this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetField {
    AgentContextSystemPrompt,
}

impl TargetField {
    pub fn collection(&self) -> Collection {
        match self {
            Self::AgentContextSystemPrompt => Collection::AgentContext,
        }
    }

    pub fn field_name(&self) -> &'static str {
        match self {
            Self::AgentContextSystemPrompt => "system_prompt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub field: TargetField,
    pub owner: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenDocument {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub digest: String,
}

/// The owner's full desired configuration, in canonical projection form.
pub type Closure = Vec<(Collection, Value)>;

/// Whether documents of `collection` belong to the frozen closure. An eval
/// definition is the instrument, not the subject: it is frozen separately in
/// `JobOrigin::definition`, and editing it is `definition_changed`.
pub(crate) fn is_closure_collection(collection: Collection) -> bool {
    collection != Collection::EvalDefinition
}

/// Read the owner's configuration inside `txn`, ordered so two reads of the
/// same state produce the same closure.
pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure> {
    let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let mut closure: Closure = references
        .documents()
        .filter(|((collection, _), _)| is_closure_collection(*collection))
        .map(|((collection, _), value)| (*collection, value.clone()))
        .collect();
    closure.sort_by_key(|(collection, value)| {
        (
            *collection,
            document_id(*collection, value).unwrap_or_default(),
        )
    });
    Ok(closure)
}

fn document_id(collection: Collection, value: &Value) -> Option<String> {
    value
        .get(collection.unique_field())?
        .as_str()
        .map(str::to_owned)
}

fn document_owner(value: &Value) -> Option<String> {
    value.get("agent_did")?.as_str().map(str::to_owned)
}

pub fn closure_digests(closure: &Closure) -> Result<Vec<FrozenDocument>> {
    closure
        .iter()
        .map(|(collection, value)| {
            Ok(FrozenDocument {
                collection: *collection,
                owner: document_owner(value).context("closure document has no agent_did")?,
                id: document_id(*collection, value)
                    .context("closure document has no logical ID")?,
                digest: desired_state_document_digest(value)?,
            })
        })
        .collect()
}

fn target_index(closure: &Closure, target: &Target) -> Result<usize> {
    closure
        .iter()
        .position(|(collection, value)| {
            *collection == target.field.collection()
                && document_id(*collection, value).as_deref() == Some(target.id.as_str())
                && document_owner(value).as_deref() == Some(target.owner.as_str())
        })
        .with_context(|| {
            format!(
                "target {} {:?}/{:?} is not in the baseline closure",
                target.field.collection().graphql_type(),
                target.owner,
                target.id
            )
        })
}

pub fn current_text(closure: &Closure, target: &Target) -> Result<String> {
    let (_, value) = &closure[target_index(closure, target)?];
    Ok(value
        .get(target.field.field_name())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// The closure with exactly one field of exactly one document replaced.
pub fn apply_text(closure: &Closure, target: &Target, text: &str) -> Result<Closure> {
    let index = target_index(closure, target)?;
    let mut patched = closure.clone();
    patched[index]
        .1
        .as_object_mut()
        .context("target document is not an object")?
        .insert(
            target.field.field_name().to_owned(),
            Value::String(text.to_owned()),
        );
    Ok(patched)
}

pub fn target_digest(closure: &Closure, target: &Target) -> Result<String> {
    desired_state_document_digest(&closure[target_index(closure, target)?].1)
}

/// Every frozen document as a digest precondition. Promotion expects the whole
/// closure and writes one document of it.
pub fn expectations(frozen: &[FrozenDocument]) -> Vec<DesiredStateExpectation> {
    frozen
        .iter()
        .map(|document| DesiredStateExpectation {
            collection: document.collection,
            owner: document.owner.clone(),
            id: document.id.clone(),
            digest: Some(document.digest.clone()),
        })
        .collect()
}

/// A plan that writes only `target` out of `closure`, guarded by `frozen`.
pub fn target_plan(
    closure: &Closure,
    target: &Target,
    frozen: &[FrozenDocument],
) -> Result<DesiredStateApplyPlan> {
    let (collection, value) = &closure[target_index(closure, target)?];
    DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: *collection,
        add: value.clone(),
        update: value.clone(),
    }])?
    .with_expected(expectations(frozen))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "did:key:target-owner";

    fn context(prompt: &str) -> Value {
        json!({
            "context_id": "monitor-context",
            "agent_did": OWNER,
            "display_name": "Monitor",
            "system_prompt": prompt,
        })
    }

    fn closure(prompt: &str) -> Closure {
        vec![
            (
                Collection::AgentBehavior,
                json!({
                    "behavior_id": "monitor",
                    "agent_did": OWNER,
                    "context_id": "monitor-context",
                    "inference_profile_id": "local",
                }),
            ),
            (Collection::AgentContext, context(prompt)),
        ]
    }

    fn target() -> Target {
        Target {
            field: TargetField::AgentContextSystemPrompt,
            owner: OWNER.into(),
            id: "monitor-context".into(),
        }
    }

    #[test]
    fn the_patch_replaces_one_field_of_one_document_and_nothing_else() {
        let before = closure("Watch the mailbox.\n");
        assert_eq!(
            current_text(&before, &target()).unwrap(),
            "Watch the mailbox.\n"
        );

        let after = apply_text(&before, &target(), "Watch the mailbox, and say why.\n").unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0], before[0], "the behavior is untouched");
        assert_eq!(
            current_text(&after, &target()).unwrap(),
            "Watch the mailbox, and say why.\n"
        );
        let mut masked = after[1].1.clone();
        masked["system_prompt"] = before[1].1["system_prompt"].clone();
        assert_eq!(masked, before[1].1, "no other field of the target moved");
        assert_ne!(
            target_digest(&after, &target()).unwrap(),
            target_digest(&before, &target()).unwrap()
        );
    }

    #[test]
    fn a_target_outside_the_closure_is_an_error_not_a_silent_insert() {
        let missing = Target {
            id: "no-such-context".into(),
            ..target()
        };
        let error = apply_text(&closure("a"), &missing, "b").unwrap_err();
        assert!(
            format!("{error:#}").contains("not in the baseline closure"),
            "{error:#}"
        );
        let foreign = Target {
            owner: "did:key:someone-else".into(),
            ..target()
        };
        assert!(current_text(&closure("a"), &foreign).is_err());
    }

    #[test]
    fn the_plan_writes_only_the_target_and_expects_the_whole_closure() {
        let before = closure("Watch the mailbox.\n");
        let frozen = closure_digests(&before).unwrap();
        assert_eq!(frozen.len(), 2);
        let after = apply_text(&before, &target(), "New text.\n").unwrap();
        let plan = target_plan(&after, &target(), &frozen).unwrap();
        assert_eq!(
            plan.documents().len(),
            1,
            "only the target document is written"
        );
        assert_eq!(plan.documents()[0].collection, Collection::AgentContext);
        assert_eq!(plan.documents()[0].add, plan.documents()[0].update);
        assert_eq!(
            plan.expected().len(),
            2,
            "the whole frozen closure is expected"
        );
        assert!(plan
            .expected()
            .iter()
            .all(|expectation| expectation.digest.is_some()));
    }

    /// Ruling R2: v1 has exactly one target field.
    #[test]
    fn the_one_target_field_is_the_context_system_prompt() {
        assert_eq!(
            TargetField::AgentContextSystemPrompt.collection(),
            Collection::AgentContext
        );
        assert_eq!(
            TargetField::AgentContextSystemPrompt.field_name(),
            "system_prompt"
        );
        assert_eq!(
            serde_json::to_value(TargetField::AgentContextSystemPrompt).unwrap(),
            json!("agent_context_system_prompt"),
            "the frozen wire form of the target field"
        );
        assert!(
            serde_json::from_value::<TargetField>(json!("task_prompt_template")).is_err(),
            "no second target field exists in v1"
        );
    }

    /// Finding F5: a definition is frozen in `JobOrigin::definition`, never in
    /// the closure, so editing it is `definition_changed` and not drift.
    #[test]
    fn eval_definitions_never_enter_the_closure() {
        assert!(!is_closure_collection(Collection::EvalDefinition));
        assert!(is_closure_collection(Collection::AgentContext));
        assert!(is_closure_collection(Collection::AgentBehavior));
    }
}
