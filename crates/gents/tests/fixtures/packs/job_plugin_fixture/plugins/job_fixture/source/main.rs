// Table-driven job guest: prints the `output` of the first `args.plan`
// entry whose `when` matches the step input, so a test can script any job
// step sequence. Spellings are those of `gents_protocol::job`; serde_json
// values stand in for the types because a fixture plugin is not a workspace
// member and cannot reach that crate.
//
// An entry is `{when, output, control?}`:
// - `when` (all keys optional): `step` (0 when `pin` is null, else 1),
//   `control` (the echoed kind, or "none"), `children` (slot -> state).
// - `control`: a non-job control request to append (a `result` output only).

use serde_json::{json, Value};
use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("reading stdin");
    let input: Value = serde_json::from_str(&input).expect("step input is JSON");

    let plan = input["args"]["plan"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let output = match plan.iter().find(|entry| matches(&entry["when"], &input)) {
        Some(entry) => render(entry),
        None => json!({"fail": {
            "code": "fixture_no_plan_match",
            "message": "no args.plan entry matches this step",
        }}),
    };
    std::io::stdout()
        .write_all(output.to_string().as_bytes())
        .expect("writing stdout");
}

fn matches(when: &Value, input: &Value) -> bool {
    if let Some(step) = when.get("step") {
        if *step != json!(u8::from(!input["pin"].is_null())) {
            return false;
        }
    }
    if let Some(kind) = when.get("control") {
        let echoed = input["control"]["kind"].as_str().unwrap_or("none");
        if kind.as_str() != Some(echoed) {
            return false;
        }
    }
    when.get("children")
        .and_then(Value::as_object)
        .is_none_or(|slots| {
            slots
                .iter()
                .all(|(slot, state)| input["children"][slot]["state"] == *state)
        })
}

fn render(entry: &Value) -> Value {
    let mut output = entry["output"].clone();
    if output.get("result").is_some() {
        if let Some(control) = entry.get("control") {
            push(&mut output, "control", control.clone());
        }
    }
    output
}

fn push(output: &mut Value, key: &str, item: Value) {
    let list = output[key].take();
    let mut list = list.as_array().cloned().unwrap_or_default();
    list.push(item);
    output[key] = Value::Array(list);
}
