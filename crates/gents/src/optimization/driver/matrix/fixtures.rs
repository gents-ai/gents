use serde_json::{json, Value};

use crate::eval::runner::freeze::tests::OWNER;
use crate::eval::runner::{ScriptKey, ScriptedExecutor, TrialEvidence};
use crate::eval::TrialUsage;
use crate::optimization::job::Budgets;
use crate::optimization::policy::PolicyV2;
use crate::optimization::proposer::ScriptedProposer;
use crate::Collection;

pub(crate) const VALIDATION_CASES: [&str; 6] =
    ["val-a", "val-b", "val-c", "val-d", "val-e", "val-f"];
pub(crate) const HELD_OUT_CASES: [&str; 6] = ["ho-a", "ho-b", "ho-c", "ho-d", "ho-e", "ho-f"];
pub(super) const TRAIN_CASES: [&str; 1] = ["train-a"];
pub(super) const TRIALS_PER_CASE: u32 = 2;
pub(crate) const BASELINE_PROMPT: &str = "Watch the mailbox.\n";
pub(crate) const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

pub(crate) const DEFINITION: &str = "monitor-findings";
pub(super) const SMALL_DEFINITION: &str = "monitor-small";
pub(super) const FEEDBACK_DEFINITION: &str = "monitor-feedback";
pub(super) const FEEDBACK: &str = "name the collection";

fn case(case_id: &str, split: &str, check: &str) -> Value {
    let params = if check == "captured_rows_count" {
        json!({"name": "findings", "min": 1})
    } else {
        json!({})
    };
    json!({
        "case_id": case_id,
        "split": split,
        "stages": [{
            "stage_id": "check",
            "prompt": "Run the monitor.",
            "deadline_secs": 600,
            "checks": [{"check": check, "params": params, "tier": "acceptance"}],
        }],
    })
}

pub(crate) fn definition(
    definition_id: &str,
    check: &str,
    validation: &[&str],
    version: i64,
) -> Value {
    let mut cases: Vec<Value> = TRAIN_CASES
        .iter()
        .map(|id| case(id, "train", check))
        .collect();
    cases.extend(validation.iter().map(|id| case(id, "validation", check)));
    cases.extend(HELD_OUT_CASES.iter().map(|id| case(id, "held_out", check)));
    json!({
        "definition_id": definition_id,
        "node_did": OWNER,
        "comparability_version": version,
        "subject": {"kind": "agent", "inference_slots": ["primary"]},
        "cases": cases,
    })
}

pub(super) fn behavior(display_name: &str) -> (Collection, Value) {
    (
        Collection::Agent,
        json!({
            "agent_id": "monitor",
            "node_did": OWNER,
            "display_name": display_name,
            "context_id": "monitor-context",
            "inference_profile_id": "local",
        }),
    )
}

pub(super) fn context(prompt: &str) -> (Collection, Value) {
    (
        Collection::AgentContext,
        json!({
            "context_id": "monitor-context",
            "node_did": OWNER,
            "display_name": "Monitor",
            "system_prompt": prompt,
            "tools_id": "monitor-tools",
        }),
    )
}

pub(super) fn tools() -> (Collection, Value) {
    (
        Collection::Tools,
        json!({
            "tools_id": "monitor-tools",
            "node_did": OWNER,
            "display_name": "Monitor tools",
            "host": {"bash": {"mode": "Off"}},
        }),
    )
}

pub(crate) fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

pub(crate) fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

pub(super) fn with_usage(evidence: TrialEvidence, tokens: u64) -> TrialEvidence {
    TrialEvidence::new(
        evidence.locator,
        evidence.stages,
        TrialUsage {
            input_tokens: Some(tokens),
            output_tokens: Some(0),
        },
        evidence.anchor,
    )
}

pub(crate) fn script(
    executor: ScriptedExecutor,
    cell: &str,
    cases: &[&str],
    evidence: impl Fn(&str) -> TrialEvidence,
) -> ScriptedExecutor {
    script_attempts(executor, cell, cases, &[1], evidence)
}

pub(super) fn script_attempts(
    mut executor: ScriptedExecutor,
    cell: &str,
    cases: &[&str],
    attempts: &[u32],
    evidence: impl Fn(&str) -> TrialEvidence,
) -> ScriptedExecutor {
    for case_id in cases {
        for trial_index in 0..TRIALS_PER_CASE {
            for attempt in attempts {
                let key = ScriptKey {
                    cell_label: cell.into(),
                    case_id: (*case_id).into(),
                    trial_index,
                    attempt: *attempt,
                };
                executor = executor.with(key, evidence(case_id));
            }
        }
    }
    executor
}

pub(crate) fn base_executor() -> ScriptedExecutor {
    ScriptedExecutor::new().with_default(pass())
}

pub(crate) fn budgets(max_case_trials: u64) -> Budgets {
    Budgets {
        max_rounds: 3,
        max_case_trials,
        max_tokens: u64::MAX,
        deadline_unix_secs: None,
    }
}

pub(crate) fn policy() -> PolicyV2 {
    PolicyV2::uncalibrated()
}

pub(crate) fn repeating_proposer(text: &str) -> ScriptedProposer {
    ScriptedProposer::new(vec![
        (
            text.to_owned(),
            "name the collection the monitor should read".to_owned()
        );
        3
    ])
}
