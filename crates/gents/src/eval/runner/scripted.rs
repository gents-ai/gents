use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScriptKey {
    pub cell_label: String,
    pub case_id: String,
    pub trial_index: u32,
    pub attempt: u32,
}

pub struct ScriptedExecutor {
    table: RapidHashMap<ScriptKey, TrialEvidence>,
    default: Option<TrialEvidence>,
    pub calls: Mutex<Vec<ScriptKey>>,
}

impl ScriptedExecutor {
    pub fn new() -> Self {
        Self {
            table: RapidHashMap::new(),
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
                prods: 0,
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

    pub fn not_evidence(did: &str) -> TrialEvidence {
        TrialEvidence::infrastructure(locator(did))
    }

    pub fn failed_evidence(
        did: &str,
        stage_id: &str,
        kind: OutcomeKind,
        reason: Option<ProviderReason>,
    ) -> TrialEvidence {
        evidence(
            locator(did),
            vec![StageEvidence {
                prods: 0,
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
        trial_node_did: did.to_string(),
        session_id: String::new(),
        home_hint: None,
    }
}

fn unprovisioned() -> TrialLocator {
    locator("did:unprovisioned")
}

fn evidence(locator: TrialLocator, stages: Vec<StageEvidence>) -> TrialEvidence {
    let anchor = Anchor {
        terminal_states: stages
            .iter()
            .filter_map(|stage| stage.terminal_state)
            .collect(),
        requests: stages.len() as u32,
        inference_calls: 0,
    };
    TrialEvidence::new(locator, stages, TrialUsage::default(), anchor)
}
