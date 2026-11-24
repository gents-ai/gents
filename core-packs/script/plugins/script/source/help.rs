use serde_json::{json, Value};

pub const TOPICS: &[(&str, &str, &str)] = &[
    (
        "tools",
        "listing, describing and batching tool calls, and their errors",
        include_str!("../help/tools.md"),
    ),
    (
        "files",
        "reading files as data and writing them",
        include_str!("../help/files.md"),
    ),
    (
        "runs",
        "starting a run and waiting for its result",
        include_str!("../help/runs.md"),
    ),
    (
        "background",
        "running a script in the background",
        include_str!("../help/background.md"),
    ),
];

fn listing() -> String {
    let mut out = String::from("Call with {\"help\": \"<topic>\"} for one of these topics:\n");
    for (name, about, _) in TOPICS {
        out.push_str(&format!("- {name}: {about}\n"));
    }
    out
}

pub fn answer(topic: &str) -> Result<Value, String> {
    let topic = topic.trim();
    let text = if topic == "topics" {
        listing()
    } else {
        TOPICS
            .iter()
            .find(|(name, _, _)| *name == topic)
            .map(|(_, _, text)| (*text).to_owned())
            .ok_or_else(|| {
                let names: Vec<&str> = TOPICS.iter().map(|(name, _, _)| *name).collect();
                format!(
                    "help topic {topic:?} is not known; use topics, {}",
                    names.join(", ")
                )
            })?
    };
    Ok(json!({"help": topic, "text": text}))
}
