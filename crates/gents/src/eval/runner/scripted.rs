//! A [`TrialExecutor`] that answers from a table instead of running anything.
//!
//! The runner loop is the thing under test in most of the suite: which trials
//! it plans, how it retries, what it writes. The scripted executor lets those
//! tests state the evidence a trial produced, keyed by the cell, case, index
//! and attempt that produced it, and records every key it was asked for.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::eval::runner::executor::{
    Capture, CaptureResult, Isolation, StageEvidence, TrialEvidence, TrialExecutor, TrialLocator,
    TrialSpec,
};
use crate::eval::{Anchor, OutcomeKind, ProviderReason, TrialUsage};

/// The slot a scripted answer belongs to. `cell_label` rather than `cell_id`,
/// so a test names the cell the way its fixture does.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScriptKey {
    pub cell_label: String,
    pub case_id: String,
    pub trial_index: u32,
    pub attempt: u32,
}

pub struct ScriptedExecutor {
    table: HashMap<ScriptKey, TrialEvidence>,
    default: Option<TrialEvidence>,
    /// Every key [`TrialExecutor::execute`] was called with, in order.
    pub calls: Mutex<Vec<ScriptKey>>,
}

impl ScriptedExecutor {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            default: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn with(mut self, key: ScriptKey, evidence: TrialEvidence) -> Self {
        self.table.insert(key, evidence);
        self
    }

    pub fn with_default(mut self, evidence: TrialEvidence) -> Self {
        self.default = Some(evidence);
        self
    }

    /// One completed stage whose `capture_name` capture holds `rows`.
    pub fn passed_evidence(
        did: &str,
        stage_id: &str,
        capture_name: &str,
        rows: Vec<Value>,
    ) -> TrialEvidence {
        let mut captures = BTreeMap::new();
        captures.insert(capture_name.to_string(), CaptureResult::Documents { rows });
        evidence(
            locator(did),
            vec![StageEvidence {
                stage_id: stage_id.to_string(),
                request_id: None,
                terminal_state: Some(RequestLifecycleState::Completed),
                failure_kind: None,
                provider_reason: None,
                messages: Vec::new(),
                tool_calls: Vec::new(),
                inference_calls: Vec::new(),
                captures,
            }],
        )
    }

    /// A trial that produced no evidence at all.
    pub fn not_evidence(did: &str) -> TrialEvidence {
        TrialEvidence::infrastructure(locator(did))
    }

    /// One stage that ended in `kind`.
    pub fn failed_evidence(
        did: &str,
        stage_id: &str,
        kind: OutcomeKind,
        reason: Option<ProviderReason>,
    ) -> TrialEvidence {
        evidence(
            locator(did),
            vec![StageEvidence {
                stage_id: stage_id.to_string(),
                request_id: None,
                terminal_state: Some(RequestLifecycleState::Failed),
                failure_kind: Some(kind),
                provider_reason: reason,
                messages: Vec::new(),
                tool_calls: Vec::new(),
                inference_calls: Vec::new(),
                captures: BTreeMap::new(),
            }],
        )
    }

    /// The scripted answer for `spec`: its keyed entry, else the default.
    fn scripted(&self, spec: &TrialSpec) -> Option<&TrialEvidence> {
        spec.script_key
            .as_ref()
            .and_then(|key| self.table.get(key))
            .or(self.default.as_ref())
    }
}

impl Default for ScriptedExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TrialExecutor for ScriptedExecutor {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    fn wants_script_key(&self) -> bool {
        true
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        self.scripted(spec)
            .map(|evidence| evidence.locator.clone())
            .unwrap_or_else(unprovisioned)
    }

    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        if let Some(key) = spec.script_key.as_ref() {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(key.clone());
            }
        }
        self.scripted(spec)
            .cloned()
            .unwrap_or_else(|| TrialEvidence::infrastructure(unprovisioned()))
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

fn locator(did: &str) -> TrialLocator {
    TrialLocator {
        trial_agent_did: did.to_string(),
        session_id: String::new(),
        home_hint: None,
    }
}

fn unprovisioned() -> TrialLocator {
    locator("did:unprovisioned")
}

/// Evidence whose anchor and digest follow from the stages it holds.
fn evidence(locator: TrialLocator, stages: Vec<StageEvidence>) -> TrialEvidence {
    let anchor = Anchor {
        terminal_states: stages
            .iter()
            .filter_map(|stage| stage.terminal_state)
            .collect(),
        requests: stages.len() as u32,
        inference_calls: 0,
    };
    let usage = TrialUsage::default();
    let evidence_digest = TrialEvidence::digest(&stages, &usage, &anchor);
    TrialEvidence {
        locator,
        stages,
        usage,
        anchor,
        evidence_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::executor::{TrialExecutor, TrialSpec};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn the_scripted_executor_returns_the_table_entry_or_the_default_and_records_calls() {
        let key = ScriptKey {
            cell_label: "base".into(),
            case_id: "k".into(),
            trial_index: 0,
            attempt: 1,
        };
        let ex = ScriptedExecutor::new()
            .with(key.clone(), ScriptedExecutor::not_evidence("did:x"))
            .with_default(ScriptedExecutor::passed_evidence(
                "did:x",
                "s",
                "items",
                vec![],
            ));
        assert!(ex.wants_script_key());
        let mut spec = TrialSpec::empty_for_tests("t1");
        spec.script_key = Some(key.clone());
        assert_eq!(
            ex.execute(&spec, CancellationToken::new())
                .await
                .stages
                .len(),
            0
        );
        spec.script_key = Some(ScriptKey {
            attempt: 2,
            ..key.clone()
        });
        assert_eq!(
            ex.execute(&spec, CancellationToken::new())
                .await
                .stages
                .len(),
            1
        );
        assert_eq!(ex.calls.lock().unwrap().len(), 2);
    }
}
