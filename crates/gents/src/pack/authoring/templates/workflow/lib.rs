//! __NAME__: a workflow shipped as a job plugin. The host calls [`step`] once per step with
//! its view of the run (`JobStepInput`) and acts on the one value returned: `effects` to
//! start or steer children, `job_calls` to read a completed child's text, and `result`,
//! `suspend` or `fail` to end the run. This skeleton spawns the pack's worker once, reads
//! its reply and finishes; grow `step0` and `advance` into your phases.
pub mod wire;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wire::*;

pub const NAME: &str = "__NAME__";
const TITLE: &str = "__TITLE__";
/// The AgentTarget the pack ships; `gents pack wire --targets-from` puts it in the job block.
const TARGET: &str = "__TARGET__";
const SLOT: &str = "worker";
const DEFAULT_BUDGET: u64 = 8;
const DEFAULT_TASK: &str = "Reply with the single word pong and nothing else.";
/// What the pin names as this workflow's source. A call without `argv`, `script` or `path`
/// is `installed` from the artifact's own provenance, so no file of this name ships.
const IDENTITY: &str = "__NAME__ 0.1.0";

/// A `fail` output.
pub fn fail(code: &str, message: &str) -> Value {
    json!({ "fail": { "code": code, "message": message } })
}

/// One step: the output for `input`, or a `fail` when the input is not a job step.
pub fn step(raw: Value) -> Value {
    let input: JobStepInput = match serde_json::from_value(raw) {
        Ok(input) => input,
        Err(e) => return fail("workflow_invalid_input", &format!("the input is not a job step ({e})")),
    };
    match input.pin.clone() {
        None => step0(&input),
        Some(pin) => advance(&input, &pin),
    }
}

fn out(o: JobStepOutput) -> Value {
    serde_json::to_value(o).expect("output serializes")
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn arg<'a>(input: &'a JobStepInput, key: &str) -> Option<&'a str> {
    input.args.as_ref()?.get(key)?.as_str()
}

/// Step 0: pin the run and start the worker on `args.task`.
fn step0(input: &JobStepInput) -> Value {
    let digest = sha256_hex(IDENTITY);
    let pin = JobPin {
        script_id: digest.clone(),
        source_digest: format!("sha256:{digest}"),
        agent_budget: input.agent_budget.unwrap_or(DEFAULT_BUDGET),
        handle: arg(input, "handle").unwrap_or(NAME).to_owned(),
        host: Default::default(),
    };
    let spawn = Spawn {
        slot: SLOT.into(),
        attempt: 0,
        target: TARGET.into(),
        prompt: arg(input, "task").unwrap_or(DEFAULT_TASK).to_owned(),
        title: SLOT.into(),
        cwd: input.caller.cwd.clone(),
    };
    out(JobStepOutput::Effects {
        effects: Effects {
            pin: Some(pin.clone()),
            spawn: Some(vec![spawn]),
            progress: Some(progress(input, &pin, JobState::Running, PhaseState::Active, "worker started")),
            ..Default::default()
        },
    })
}

/// Every later step: wait while the worker is live, read its text once it has completed,
/// then finish with its reply.
fn advance(input: &JobStepInput, pin: &JobPin) -> Value {
    let Some(worker) = input.children.get(SLOT) else {
        return fail("workflow_launch_failed", "the worker slot is missing from the run");
    };
    if worker.live() {
        return out(JobStepOutput::Effects {
            effects: Effects {
                progress: Some(progress(input, pin, JobState::Running, PhaseState::Active, "waiting on the worker")),
                ..Default::default()
            },
        });
    }
    if worker.state != "completed" {
        return out(JobStepOutput::Result {
            result: json!({ "reply": null, "limitation": format!("the worker ended {}", worker.state) }),
            scratch: None,
            progress: progress(input, pin, JobState::Completed, PhaseState::Failed, "the worker did not answer"),
        });
    }
    match worker_text(input) {
        Err(failure) => failure,
        Ok(None) => json!({ "job_calls": { "requests": [JobCall {
            id: "text".into(),
            child_output: ChildOutputRequest { slots: vec![SLOT.into()], offset: 0, limit: input.ceiling.result_bytes },
        }] } }),
        Ok(Some(text)) => out(JobStepOutput::Result {
            result: json!({ "reply": text.trim() }),
            scratch: None,
            progress: progress(input, pin, JobState::Completed, PhaseState::Done, "done"),
        }),
    }
}

/// The worker's text from the previous round's `child_output` answer, if a round ran.
fn worker_text(input: &JobStepInput) -> Result<Option<String>, Value> {
    let Some(answer) = input.job_results.as_ref().and_then(|r| r.get("text")) else {
        return Ok(None);
    };
    match serde_json::from_value::<RoundResult<ChildOutputResponse>>(answer.clone()) {
        Ok(RoundResult::Result { result }) => Ok(result.slots.get(SLOT).map(|s| s.text.clone())),
        Ok(RoundResult::Error { error }) => Err(fail(
            "workflow_launch_failed",
            &format!("the host could not read the worker's output ({error})"),
        )),
        Err(e) => Err(fail(
            "workflow_invalid_input",
            &format!("the child_output answer is not of the asked shape ({e})"),
        )),
    }
}

fn progress(input: &JobStepInput, pin: &JobPin, state: JobState, phase: PhaseState, event: &str) -> JobProgress {
    let spawned = input.children.len().max(1) as u64;
    JobProgress {
        title: TITLE.into(),
        state,
        pause_reason: None,
        phases: vec![Phase { title: "Worker".into(), state: phase }],
        active: input
            .children
            .iter()
            .filter(|(_, c)| c.live())
            .map(|(slot, c)| ActiveChild { slot: slot.clone(), title: c.title.clone(), session_id: c.session_id.clone() })
            .collect(),
        spawned,
        budget: ProgressBudget {
            total: pin.agent_budget,
            spent: spawned,
            remaining: pin.agent_budget.saturating_sub(spawned),
        },
        last_event: event.into(),
        last_event_at: Value::Null,
        observed_terminal_children: input.children.values().filter(|c| !c.live()).count() as u64,
        revision: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step0_matches_the_shipped_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/00_step0_spawns_worker.json")).unwrap();
        assert_eq!(step(fixture["input"].clone()), fixture["expect"]);
    }

    #[test]
    fn a_completed_worker_is_read_in_one_round_and_then_finishes() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/00_step0_spawns_worker.json")).unwrap();
        let mut input = fixture["input"].clone();
        input["pin"] = fixture["expect"]["effects"]["pin"].clone();
        input["children"] = json!({ SLOT: { "state": "completed", "title": SLOT } });
        let round = step(input.clone());
        assert_eq!(round["job_calls"]["requests"][0]["child_output"]["slots"], json!([SLOT]));
        input["job_results"] = json!({ "text": { "result": { "slots": { SLOT: { "text": "pong\n" } } } } });
        let done = step(input);
        assert_eq!(done["result"], json!({ "reply": "pong" }));
        assert_eq!(done["progress"]["state"], "completed");
    }
}
