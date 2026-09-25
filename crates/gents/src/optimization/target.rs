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

/// The first way a baseline pack's configuration differs from the live
/// configuration a job would promote into. A job evaluates the frozen live
/// revision (#1455), not a transfer to it, so a pack that installs any other
/// configuration beside the target prompt cannot be a job's baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineMismatch {
    pub collection: Collection,
    pub id: String,
    /// The first differing field, or `None` when the live configuration has
    /// no such document.
    pub field: Option<String>,
}

impl std::fmt::Display for BaselineMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.collection.graphql_type();
        match &self.field {
            Some(field) => write!(
                formatter,
                "the baseline pack's {name} {:?} differs from the live one in {field}; supply a pack exported from this configuration",
                self.id
            ),
            None => write!(
                formatter,
                "the baseline pack declares {name} {:?}, which the live configuration does not have; supply a pack exported from this configuration",
                self.id
            ),
        }
    }
}

impl std::error::Error for BaselineMismatch {}

/// Whether a trial replaces `field` of a pack document (`None`: the whole
/// document) when it installs the baseline pack, so the pack may differ from
/// the live configuration there and still be the same subject. These are the
/// only such remaps:
///
/// - The `AgentPrincipal`: a trial installs the pack's principal as its own
///   fresh principal and names the subject behavior explicitly, so no
///   principal setting of the live owner is part of what a trial runs.
/// - An `AgentBehavior`'s `inference_profile_id` that names an inference slot:
///   the trial binds every slot to the eval target's profile, never to the
///   live binding.
/// - `tags`: installing a pack stamps its provenance tag and merges retained
///   discovery tags, the trial's install as much as the live one, and tags
///   never select what executes (the pack owner compares immutable resources
///   without them for the same reason).
///
/// Workspace and host paths are not configuration documents: a trial
/// publishes its own `WorkspaceRoot`, which no closure holds.
fn remapped_by_trial(collection: Collection, field: Option<&str>, pack_document: &Value) -> bool {
    match (collection, field) {
        (Collection::AgentPrincipal, _) => true,
        (_, Some("tags")) => true,
        (Collection::AgentBehavior, Some(field)) => {
            field == "inference_profile_id"
                && pack_document
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with(crate::pack::INFERENCE_SLOT_REFERENCE_PREFIX))
        }
        _ => false,
    }
}

/// `document` without the fields a trial remaps, in the digest's canonical
/// form.
fn comparable(collection: Collection, document: &Value, pack_document: &Value) -> Result<Value> {
    let (_, projected) = crate::config_client::config_projection(collection, Some(document))?;
    let mut projected = projected.context("canonical projection missing")?;
    if let Some(object) = projected.as_object_mut() {
        object.retain(|field, _| !remapped_by_trial(collection, Some(field), pack_document));
    }
    Ok(projected)
}

/// Refuse a baseline pack unless every configuration document it installs is
/// the live document of the same identity, compared by the canonical digest
/// promotion guards with, apart from [`remapped_by_trial`] fields.
pub fn baseline_equivalence(pack: &DesiredStateApplyPlan, live: &Closure) -> Result<()> {
    for document in pack.documents() {
        let collection = document.collection;
        if !is_closure_collection(collection) || remapped_by_trial(collection, None, &document.add)
        {
            continue;
        }
        let id =
            document_id(collection, &document.add).context("pack document has no logical ID")?;
        let pack_value = comparable(collection, &document.add, &document.add)?;
        let live_value = live
            .iter()
            .find(|(candidate, value)| {
                *candidate == collection && document_id(*candidate, value).as_deref() == Some(&id)
            })
            .map(|(_, value)| comparable(collection, value, &document.add))
            .transpose()?;
        let Some(live_value) = live_value else {
            return Err(BaselineMismatch {
                collection,
                id,
                field: None,
            }
            .into());
        };
        if desired_state_document_digest(&pack_value)?
            == desired_state_document_digest(&live_value)?
        {
            continue;
        }
        let field = first_differing_field(&pack_value, &live_value)
            .unwrap_or_else(|| "its canonical form".to_owned());
        return Err(BaselineMismatch {
            collection,
            id,
            field: Some(field),
        }
        .into());
    }
    Ok(())
}

/// The first root field, in name order, whose values differ once an absent,
/// null or empty-list value is read as unset, as the digest reads them.
fn first_differing_field(left: &Value, right: &Value) -> Option<String> {
    fn unset(value: Option<&Value>) -> Option<&Value> {
        value.filter(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
    }
    let (left, right) = (left.as_object()?, right.as_object()?);
    let fields: std::collections::BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    fields
        .into_iter()
        .find(|field| unset(left.get(*field)) != unset(right.get(*field)))
        .cloned()
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
