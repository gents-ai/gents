//! A serialized [`TrialSpec`] never carries checks, tiers, splits or other
//! cases; the one field that names a case is [`TrialSpec::script_key`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::document_config::EvalCapture;
use crate::eval::runner::embedded::observe::{
    InferenceCallEvidence, MessageEvidence, ToolCallEvidence,
};
use crate::eval::runner::progress::StageProgress;
use crate::eval::runner::scripted::ScriptKey;
use crate::eval::{Anchor, OutcomeKind, ProviderReason, TrialUsage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Isolation {
    Embedded,
    Process,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialSpec {
    pub trial_id: String,
    pub pack_dir: PathBuf,
    pub pack_digest: String,
    pub agent_id: String,
    pub inference: InferenceBinding,
    pub fixtures: TrialFixtures,
    #[serde(default)]
    pub host_bash: bool,
    pub stages: Vec<StageSpec>,
    pub trial_dir: PathBuf,
    pub script_key: Option<ScriptKey>,
    #[serde(skip)]
    pub progress: StageProgress,
    #[serde(skip)]
    pub review: super::review::StageReview,
}

impl TrialSpec {
    #[cfg(test)]
    pub(crate) fn empty_for_tests(trial_id: &str) -> Self {
        Self {
            trial_id: trial_id.to_string(),
            pack_dir: PathBuf::new(),
            pack_digest: String::new(),
            agent_id: String::new(),
            inference: InferenceBinding {
                profile: Value::Null,
                backend: Value::Null,
                sampling: None,
                execution: None,
                retry_policy: None,
                seed: 0,
            },
            fixtures: TrialFixtures::default(),
            host_bash: false,
            stages: Vec::new(),
            trial_dir: PathBuf::new(),
            script_key: None,
            progress: StageProgress::default(),
            review: Default::default(),
        }
    }
}

/// Copied verbatim so a later edit cannot change what a finished trial meant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceBinding {
    pub profile: Value,
    pub backend: Value,
    pub sampling: Option<Value>,
    pub execution: Option<Value>,
    pub retry_policy: Option<Value>,
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
    /// Written instead of submitting `prompt`.
    pub seed: Option<FixtureDocument>,
    pub deadline_secs: u64,
    #[serde(default)]
    pub settle: bool,
    #[serde(default)]
    pub continuation: Option<crate::document_config::EvalContinuation>,
    #[serde(default)]
    pub review_previous: bool,
    pub captures: Vec<Capture>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Capture {
    Schema {
        name: String,
        collections: Vec<String>,
    },
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

impl From<&EvalCapture> for Capture {
    fn from(capture: &EvalCapture) -> Self {
        match capture {
            EvalCapture::Schema { name, collections } => Self::Schema {
                name: name.clone(),
                collections: collections.clone(),
            },
            EvalCapture::Documents {
                name,
                collection,
                filter,
                fields,
            } => Self::Documents {
                name: name.clone(),
                collection: collection.clone(),
                filter: filter.clone(),
                fields: fields.clone(),
            },
            EvalCapture::File { name, glob } => Self::File {
                name: name.clone(),
                glob: glob.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialLocator {
    pub trial_node_did: String,
    pub session_id: String,
    /// A locator only. Never identity.
    pub home_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageEvidence {
    pub stage_id: String,
    pub request_id: Option<String>,
    pub terminal_state: Option<RequestLifecycleState>,
    pub failure_kind: Option<OutcomeKind>,
    pub provider_reason: Option<ProviderReason>,
    pub messages: Vec<MessageEvidence>,
    pub tool_calls: Vec<ToolCallEvidence>,
    pub inference_calls: Vec<InferenceCallEvidence>,
    pub captures: BTreeMap<String, CaptureResult>,
    #[serde(default)]
    pub prods: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureResult {
    Schema {
        collections: Vec<Value>,
    },
    Documents {
        rows: Vec<Value>,
    },
    Files {
        files: Vec<FileRef>,
        /// Recorded rather than dropped, so a check can tell an empty capture
        /// from files refused outside the trial's workspace; defaulted, so
        /// older evidence still reads.
        #[serde(default)]
        outside_workspace: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialEvidence {
    pub locator: TrialLocator,
    pub stages: Vec<StageEvidence>,
    pub usage: TrialUsage,
    pub anchor: Anchor,
    pub evidence_digest: String,
}

impl TrialEvidence {
    /// SHA-256 over canonical JSON of `(stages, usage, anchor)`.
    pub fn digest(stages: &[StageEvidence], usage: &TrialUsage, anchor: &Anchor) -> String {
        #[derive(Serialize)]
        struct DigestInput<'a> {
            stages: &'a [StageEvidence],
            usage: &'a TrialUsage,
            anchor: &'a Anchor,
        }

        // The fallbacks should be unreachable; a digest over nothing would make
        // unlike trials compare equal, so it is reported rather than absorbed.
        let value = serde_json::to_value(DigestInput {
            stages,
            usage,
            anchor,
        })
        .unwrap_or_else(|error| {
            tracing::error!(
                error = %gents_protocol::error_chain::ErrorChain(&error).to_string(),
                "eval trial evidence did not serialize; its digest covers nothing"
            );
            Value::Null
        });
        let bytes = serde_json::to_vec(&canonical(value)).unwrap_or_else(|error| {
            tracing::error!(
                error = %gents_protocol::error_chain::ErrorChain(&error).to_string(),
                "eval trial evidence digest input did not encode; its digest covers nothing"
            );
            Vec::new()
        });
        format!("{:x}", Sha256::digest(bytes))
    }

    pub fn new(
        locator: TrialLocator,
        stages: Vec<StageEvidence>,
        usage: TrialUsage,
        anchor: Anchor,
    ) -> Self {
        let evidence_digest = Self::digest(&stages, &usage, &anchor);
        Self {
            locator,
            stages,
            usage,
            anchor,
            evidence_digest,
        }
    }

    pub fn infrastructure(locator: TrialLocator) -> Self {
        Self::new(
            locator,
            Vec::new(),
            TrialUsage::default(),
            Anchor {
                terminal_states: Vec::new(),
                requests: 0,
                inference_calls: 0,
            },
        )
    }
}

/// Sorts object keys so the digest does not depend on the order a home wrote a
/// row. `serde_json` keeps insertion order on any build that enables
/// `preserve_order`, so this sort must not be removed while that build may be
/// in use.
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

#[async_trait::async_trait]
pub trait TrialExecutor: Send + Sync {
    fn isolation(&self) -> Isolation;

    fn wants_script_key(&self) -> bool {
        false
    }

    /// Creates the trial's identity (and, for embedded, its home) so the
    /// `EvalTrial` row can be written before execution. Never returns `Err`: a
    /// failed provisioning returns a locator whose `trial_node_did` is
    /// `"did:unprovisioned"`, and [`Self::execute`] then returns
    /// [`TrialEvidence::infrastructure`].
    async fn provision(&self, spec: &TrialSpec) -> TrialLocator;

    /// Release a trial that was provisioned and will never run (the embedded
    /// executor holds a booted home between [`Self::provision`] and
    /// [`Self::execute`]); the default does nothing.
    async fn discard(&self, _trial_id: &str) {}

    /// Never returns `Err`. Observes `cancel`: on cancellation, interrupts the
    /// current request and returns what it has.
    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence;

    /// Reads captures back out of a home that already ran, when it still
    /// exists. Recollected evidence is capture evidence, not gradeable
    /// evidence: a finished home records which requests a session held, not
    /// which stage submitted them, so the stages this returns are named by
    /// their position in the session (`"0"`, `"1"`, …) and match no case's
    /// `stage_id`, and `interrupted_on_deadline` is unknowable afterwards.
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
        b.locator.trial_node_did = "did:b".into();
        assert_eq!(a.evidence_digest, b.evidence_digest);
        let c = ScriptedExecutor::passed_evidence(
            "did:a",
            "s1",
            "items",
            vec![serde_json::json!({"k": 2})],
        );
        assert_ne!(a.evidence_digest, c.evidence_digest);
    }

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
