// Table-driven job guest: prints the `output` of the first `args.plan`
// entry whose `when` matches the step input, so a test can script any job
// step sequence. Spellings are those of `gents_protocol::job`; serde_json
// values stand in for the types because a fixture plugin is not a workspace
// member and cannot reach that crate.
//
// An entry is `{when, output, note?, scratch?, control?}`:
// - `when` (all keys optional): `step` (0 when `pin` is null, else 1),
//   `control` (the echoed kind, or "none"), `children` (slot -> state).
// - `note`: also write one FixtureNote holding the mounted asset's text.
// - `scratch`: also write that text as scratch (a `result` output only).
// - `control`: a non-job control request to append (a `result` output only).

use serde_json::{json, Value};
use std::io::{Read, Write};

const MOUNT_FILE: &str = "fixture_assets/a.txt";

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
        Some(entry) => render(entry, &input),
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

fn render(entry: &Value, input: &Value) -> Value {
    let mut output = entry["output"].clone();
    let text = (entry["note"] == json!(true) || entry["scratch"] == json!(true))
        .then(|| read_mount(input));
    if entry["note"] == json!(true) {
        push(
            &mut output,
            "writes",
            json!({
                "op": "insert",
                "collection": "FixtureNote",
                "fields": {"text": text},
            }),
        );
    }
    if output.get("result").is_some() {
        if entry["scratch"] == json!(true) {
            push(
                &mut output,
                "scratch",
                json!({"name": "fixture", "content": text}),
            );
        }
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

// The mount for a class is its directory, so the asset is read by the name
// below the class.
fn read_mount(input: &Value) -> String {
    let (class, file) = MOUNT_FILE.split_once('/').expect("class/file");
    let mount = input["mounts"]
        .as_array()
        .and_then(|mounts| mounts.iter().find(|m| m["class"] == class))
        .and_then(|m| m["path"].as_str())
        .expect("the fixture_assets mount");
    std::fs::read_to_string(std::path::Path::new(mount).join(file)).expect("reading the mount")
}
