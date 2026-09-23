//! The executor seam: what one trial contains, what running it yields, and the
//! trait the embedded and scripted executors implement.
//!
//! A [`TrialSpec`] is everything a trial may know. It never carries a check
//! name or parameter, a tier, a split, or another case: those belong to
//! grading, which runs after execution and reads only the evidence. The one
//! field that names a case is [`TrialSpec::script_key`], and the loop fills it
//! only for an executor that asks for it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::eval::runner::embedded::observe::{
    InferenceCallEvidence, MessageEvidence, ToolCallEvidence,
};
use crate::eval::runner::scripted::ScriptKey;
use crate::eval::{Anchor, OutcomeKind, ProviderReason, TrialUsage};

/// Where a trial's runtime lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation {
    Embedded,
    Process,
}

/// Everything a trial may contain. Never checks, tiers, splits, case ids or
/// other cases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialSpec {
    pub trial_id: String,
    /// Materialized pack for this cell.
    pub pack_dir: PathBuf,
    pub pack_digest: String,
    pub behavior_id: String,
    /// Profile and backend documents copied verbatim, plus the trial's seed.
    pub inference: InferenceBinding,
    pub fixtures: TrialFixtures,
    pub stages: Vec<StageSpec>,
    pub captures: Vec<Capture>,
    /// `<run dir>/trials/<trial_id>`.
    pub home_dir: PathBuf,
    /// Populated only when [`TrialExecutor::wants_script_key`]; the one field
    /// that carries a `case_id`.
    pub script_key: Option<ScriptKey>,
}

impl TrialSpec {
    /// A spec with nothing in it but an id, for tests that exercise the seam
    /// rather than the trial.
    #[cfg(test)]
    pub(crate) fn empty_for_tests(trial_id: &str) -> Self {
        Self {
            trial_id: trial_id.to_string(),
            pack_dir: PathBuf::new(),
            pack_digest: String::new(),
            behavior_id: String::new(),
            inference: InferenceBinding {
                profile: Value::Null,
                backend: Value::Null,
                sampling: None,
                seed: 0,
            },
            fixtures: TrialFixtures::default(),
            stages: Vec::new(),
            captures: Vec::new(),
            home_dir: PathBuf::new(),
            script_key: None,
        }
    }
}

/// The inference documents a trial runs against, copied verbatim so a later
/// edit cannot change what a finished trial meant. The seed lives on the
/// sampling document; it is repeated here because the trial is seeded by index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceBinding {
    pub profile: Value,
    pub backend: Value,
    pub sampling: Option<Value>,
    pub seed: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialFixtures {
    pub schemas: Vec<String>,
    pub documents: Vec<FixtureDocument>,
    pub files: Vec<FixtureFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureDocument {
    pub collection: String,
    pub document: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureFile {
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSpec {
    pub stage_id: String,
    pub prompt: String,
    pub deadline_secs: u64,
}

/// What to read out of a finished trial home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Capture {
    Documents {
        name: String,
        collection: String,
        filter: Value,
        fields: Vec<String>,
    },
    File {
        name: String,
        glob: String,
    },
}

/// Where a trial's durable evidence lives once it has been provisioned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialLocator {
    pub trial_agent_did: String,
    pub session_id: String,
    /// A locator only. Never identity.
    pub home_hint: Option<String>,
}

/// How one stage ran, with everything grading may read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageEvidence {
    pub stage_id: String,
    pub request_id: Option<String>,
    pub terminal_state: Option<RequestLifecycleState>,
    /// `None` means the stage ran to `Completed`.
    pub failure_kind: Option<OutcomeKind>,
    pub provider_reason: Option<ProviderReason>,
    pub messages: Vec<MessageEvidence>,
    pub tool_calls: Vec<ToolCallEvidence>,
    pub inference_calls: Vec<InferenceCallEvidence>,
    pub captures: BTreeMap<String, CaptureResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureResult {
    Documents { rows: Vec<Value> },
    Files { files: Vec<FileRef> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// What one execution produced. The digest covers the evidence itself, so two
/// runs of the same trial in different homes compare equal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialEvidence {
    pub locator: TrialLocator,
    /// Only the stages that were submitted.
    pub stages: Vec<StageEvidence>,
    pub usage: TrialUsage,
    pub anchor: Anchor,
    pub evidence_digest: String,
}

impl TrialEvidence {
    /// SHA-256 over canonical JSON of `(stages, usage, anchor)`; the locator is
    /// excluded, because where a trial ran is not what it did.
    pub fn digest(stages: &[StageEvidence], usage: &TrialUsage, anchor: &Anchor) -> String {
        #[derive(Serialize)]
        struct DigestInput<'a> {
            stages: &'a [StageEvidence],
            usage: &'a TrialUsage,
            anchor: &'a Anchor,
        }

        let value = serde_json::to_value(DigestInput {
            stages,
            usage,
            anchor,
        })
        .unwrap_or(Value::Null);
        let bytes = serde_json::to_vec(&canonical(value)).unwrap_or_default();
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Evidence for a trial that never ran: nothing observed, and an anchor
    /// that says so.
    pub fn infrastructure(locator: TrialLocator) -> Self {
        let stages = Vec::new();
        let usage = TrialUsage::default();
        let anchor = Anchor {
            terminal_states: Vec::new(),
            requests: 0,
            inference_calls: 0,
        };
        let evidence_digest = Self::digest(&stages, &usage, &anchor);
        Self {
            locator,
            stages,
            usage,
            anchor,
            evidence_digest,
        }
    }
}

/// Rebuild every object with its keys sorted, so the digest depends on the
/// content and not on the order a home happened to write a captured row in.
///
/// `serde_json` without `preserve_order` already keeps `Value::Object` in a
/// `BTreeMap`, which is how this workspace builds it today: the feature is
/// enabled only through `tree-sitter`'s build dependency, and resolver 2 does
/// not unify that into the lib build. This sort makes the digest independent
/// of that resolution, which a transitive dependency could change.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// Runs one trial. Implementations are the embedded runtime and, for tests,
/// [`crate::eval::runner::scripted::ScriptedExecutor`].
#[async_trait::async_trait]
pub trait TrialExecutor: Send + Sync {
    fn isolation(&self) -> Isolation;

    /// Only the scripted executor returns true; the loop then fills
    /// [`TrialSpec::script_key`].
    fn wants_script_key(&self) -> bool {
        false
    }

    /// Creates the trial's identity (and, for embedded, its home) so the
    /// `EvalTrial` row can be written before execution. Never returns `Err`: a
    /// failed provisioning returns a locator whose `trial_agent_did` is
    /// `"did:unprovisioned"`, and [`Self::execute`] then reports
    /// [`OutcomeKind::Infrastructure`].
    async fn provision(&self, spec: &TrialSpec) -> TrialLocator;

    /// Never returns `Err`. Observes `cancel`: on cancellation, interrupts the
    /// current request and returns what it has.
    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence;

    /// Reads captures back out of a home that already ran, when it still
    /// exists.
    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence>;
}

#[cfg(test)]
mod tests {
    use crate::eval::runner::scripted::ScriptedExecutor;

    #[test]
    fn the_evidence_digest_ignores_the_locator_and_is_stable() {
        let a = ScriptedExecutor::passed_evidence(
            "did:a",
            "s1",
            "items",
            vec![serde_json::json!({"k": 1})],
        );
        let mut b = a.clone();
        b.locator.home_hint = Some("elsewhere".into());
        b.locator.trial_agent_did = "did:b".into();
        assert_eq!(a.evidence_digest, b.evidence_digest);
        let c = ScriptedExecutor::passed_evidence(
            "did:a",
            "s1",
            "items",
            vec![serde_json::json!({"k": 2})],
        );
        assert_ne!(a.evidence_digest, c.evidence_digest);
    }

    /// Captured rows are arbitrary JSON read back out of a trial home, and
    /// `serde_json` keeps object keys in insertion order here, so two homes
    /// that produced the same row with its fields written in a different order
    /// must still digest the same.
    #[test]
    fn the_evidence_digest_ignores_the_key_order_of_captured_rows() {
        let one = ScriptedExecutor::passed_evidence(
            "did:a",
            "s1",
            "items",
            vec![serde_json::json!({"a": 1, "b": {"y": 2, "x": 3}})],
        );
        let other = ScriptedExecutor::passed_evidence(
            "did:a",
            "s1",
            "items",
            vec![serde_json::json!({"b": {"x": 3, "y": 2}, "a": 1})],
        );
        assert_eq!(one.evidence_digest, other.evidence_digest);
    }
}
