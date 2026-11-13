//! The `workflow` tool: an agent's JavaScript step function, passed as
//! `script`, run as a background job. This plugin owns the run (the pin that
//! names the script, the default budget, the error wording) and asks the host
//! to evaluate the script through the `gents::run_script` import it declares;
//! the host runs the call's own script in its embedded engine and hands the
//! answer back. One invocation is one job step: `JobStepInput` in, one
//! `JobStepOutput` out.
mod host;

pub use host::HostError;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The agent budget a run pins when its call names none: a stop-loss for an unsized script.
const DEFAULT_AGENT_BUDGET: u64 = 8;
/// The handle a run pins when its call names none.
const DEFAULT_HANDLE: &str = "workflow";
/// The answer to a call without a script, byte for byte what the stub said.
pub const NO_SCRIPT_MESSAGE: &str = "pass the workflow as `script`: a JavaScript module whose `module.exports` is the step function `(input) => output`; see the tool description";

/// What a step ends with: a JSON output for the host, or a trap (the plugin
/// exits non-zero with the message on stderr) so an engine failure keeps the
/// host's verdict and wording.
#[derive(Debug, PartialEq, Eq)]
pub enum Stepped {
    Output(Value),
    Trap(String),
}

pub fn fail(code: &str, message: &str) -> Value {
    json!({"fail": {"code": code, "message": message}})
}

/// One step through the host import.
pub fn step(input: Value) -> Stepped {
    step_with(input, host::run_script)
}

/// One step with `eval` standing in for the host import.
pub fn step_with(input: Value, eval: impl Fn(&Value) -> Result<Value, HostError>) -> Stepped {
    let Some(script) = input
        .get("script")
        .and_then(Value::as_str)
        .filter(|script| !script.is_empty())
        .map(str::to_owned)
    else {
        return Stepped::Output(fail("workflow_invalid_input", NO_SCRIPT_MESSAGE));
    };
    match eval(&input) {
        Ok(output) => Stepped::Output(with_pin(output, &input, &script)),
        Err(HostError::NoScript) => {
            Stepped::Output(fail("workflow_invalid_input", NO_SCRIPT_MESSAGE))
        }
        Err(HostError::Engine(message)) => Stepped::Trap(message),
        Err(HostError::BadInput(message)) => {
            Stepped::Trap(format!("the host could not read the step input: {message}"))
        }
        Err(HostError::TooLarge) => {
            Stepped::Trap("the script's output passed the plugin's output cap".into())
        }
    }
}

/// A run's pin names the script that ran: `script_id` and `source_digest` of
/// any pin the output carries are set to the script's digest, and a step-0
/// `effects` output that spawns but carries no `pin` gets this plugin's.
fn with_pin(mut output: Value, input: &Value, script: &str) -> Value {
    let digest = format!("{:x}", Sha256::digest(script.as_bytes()));
    for pointer in ["/pin", "/effects/pin"] {
        if let Some(pin) = output.pointer_mut(pointer).and_then(Value::as_object_mut) {
            pin.insert("script_id".into(), json!(digest));
            pin.insert("source_digest".into(), json!(format!("sha256:{digest}")));
        }
    }
    let spawns = output
        .pointer("/effects/spawn")
        .and_then(Value::as_array)
        .is_some_and(|spawns| !spawns.is_empty());
    let pinned = output
        .pointer("/effects/pin")
        .is_some_and(|pin| !pin.is_null());
    let step0 = input.get("pin").is_none_or(Value::is_null);
    if !(spawns && !pinned && step0) {
        return output;
    }
    let agent_budget = input
        .get("agent_budget")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_AGENT_BUDGET);
    let handle = input
        .pointer("/args/handle")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_HANDLE)
        .to_owned();
    if let Some(effects) = output.get_mut("effects").and_then(Value::as_object_mut) {
        effects.insert(
            "pin".into(),
            json!({
                "script_id": digest,
                "source_digest": format!("sha256:{digest}"),
                "agent_budget": agent_budget,
                "handle": handle,
            }),
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SPAWNING: &str = "module.exports = (i) => ({effects: {spawn: [{slot: \"s\", attempt: 0, target: \"t\", prompt: \"p\", title: \"t\"}]}})";

    fn sha(text: &str) -> String {
        format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes())
        )
    }

    /// An evaluator standing in for the host import: what the script answers.
    fn answers(output: Value) -> impl Fn(&Value) -> Result<Value, HostError> {
        move |_| Ok(output.clone())
    }

    #[test]
    fn a_call_that_names_no_budget_or_handle_pins_the_small_defaults() {
        let spawn = json!({"effects": {"spawn": [{"slot": "s", "attempt": 0, "target": "t", "prompt": "p", "title": "t"}]}});
        let Stepped::Output(out) = step_with(
            json!({"pin": null, "script": SPAWNING, "args": {}}),
            answers(spawn),
        ) else {
            panic!("a spawning step 0 is output")
        };
        let pin = &out["effects"]["pin"];
        assert_eq!(pin["script_id"], sha(SPAWNING));
        assert_eq!(pin["source_digest"], format!("sha256:{}", sha(SPAWNING)));
        assert_eq!(pin["agent_budget"], 8);
        assert_eq!(pin["handle"], "workflow");
    }

    #[test]
    fn a_guest_pin_gets_the_digest_and_a_later_step_keeps_its_output() {
        let pinned = json!({"effects": {"pin": {"script_id": "mine", "source_digest": "sha256:mine", "agent_budget": 1, "handle": "m"}, "spawn": [{"slot": "s", "attempt": 0, "target": "t", "prompt": "p", "title": "t"}]}});
        let Stepped::Output(step0) =
            step_with(json!({"pin": null, "script": SPAWNING}), answers(pinned))
        else {
            panic!()
        };
        let pin = &step0["effects"]["pin"];
        assert_eq!(pin["script_id"], sha(SPAWNING));
        assert_eq!(pin["source_digest"], format!("sha256:{}", sha(SPAWNING)));
        assert_eq!(pin["agent_budget"], 1);
        assert_eq!(pin["handle"], "m");
        let Stepped::Output(later) = step_with(
            json!({"pin": {"script_id": "x"}, "script": SPAWNING}),
            answers(json!({"result": {"done": true}})),
        ) else {
            panic!()
        };
        assert_eq!(later, json!({"result": {"done": true}}));
    }

    #[test]
    fn no_script_fails_with_the_stub_message() {
        let Stepped::Output(out) = step_with(json!({"args": {}}), answers(json!({}))) else {
            panic!()
        };
        assert_eq!(out["fail"]["code"], "workflow_invalid_input");
        assert_eq!(out["fail"]["message"], NO_SCRIPT_MESSAGE);
        assert!(NO_SCRIPT_MESSAGE.starts_with("pass the workflow as `script`"));
    }

    #[test]
    fn an_engine_failure_traps_with_its_message() {
        let failing = |_: &Value| Err(HostError::Engine("the script does not compile: x".into()));
        let Stepped::Trap(message) = step_with(json!({"pin": null, "script": "x"}), failing) else {
            panic!("an engine failure traps")
        };
        assert_eq!(message, "the script does not compile: x");
    }

    #[test]
    fn the_hosts_no_script_answer_is_the_stub_message() {
        let none = |_: &Value| Err(HostError::NoScript);
        let Stepped::Output(out) = step_with(json!({"pin": null, "script": "x"}), none) else {
            panic!()
        };
        assert_eq!(out["fail"]["message"], NO_SCRIPT_MESSAGE);
    }
}
