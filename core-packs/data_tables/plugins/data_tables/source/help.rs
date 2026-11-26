//! Help mode: a call with `help` names a topic and gets its text back instead
//! of running, so the tool description stays a small core and the detail is
//! read only when it is needed.
use serde_json::json;

use crate::Res;

/// Each topic: its name, what it covers, and its text.
pub const TOPICS: &[(&str, &str, &str)] = &[
    (
        "inputs",
        "path, files, tables, the three modes and the other options",
        include_str!("../help/inputs.md"),
    ),
    (
        "sql",
        "the SQL dialect, useful functions and example calls",
        include_str!("../help/sql.md"),
    ),
    (
        "csv",
        "how CSV types and headers are detected",
        include_str!("../help/csv.md"),
    ),
    (
        "output",
        "the result, its types and values, and the row order",
        include_str!("../help/output.md"),
    ),
    (
        "paging",
        "reading a long result page by page with cursor",
        include_str!("../help/paging.md"),
    ),
    (
        "saving",
        "writing a result as a CSV or Parquet file with output",
        include_str!("../help/saving.md"),
    ),
    (
        "limits",
        "memory, size limits and what fails",
        include_str!("../help/limits.md"),
    ),
];

fn listing() -> String {
    let mut out = String::from("Call with {\"help\": \"<topic>\"} for one of these topics:\n");
    for (name, about, _) in TOPICS {
        out.push_str(&format!("- {name}: {about}\n"));
    }
    out
}

/// The text of `topic`, or of the topic list for `topics`.
pub fn text(topic: &str) -> Res<String> {
    let topic = topic.trim();
    if topic == "topics" {
        return Ok(listing());
    }
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
        })
}

/// The result of a help call.
pub fn answer(topic: &str) -> Res<serde_json::Value> {
    Ok(json!({"help": topic.trim(), "text": text(topic)?}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn reply(raw: &str) -> Value {
        serde_json::from_str(&crate::run(raw).unwrap()).unwrap()
    }

    #[test]
    fn every_topic_answers_with_its_text_and_no_other_field() {
        for (name, _, text) in TOPICS {
            let v = reply(&json!({"help": name}).to_string());
            assert_eq!(v, json!({"response": {"help": name, "text": text}}));
            assert!(text.len() > 200, "{name}");
        }
    }

    #[test]
    fn the_topic_list_names_every_topic() {
        let v = reply(r#"{"help": "topics"}"#);
        let list = v["response"]["text"].as_str().unwrap();
        for (name, about, _) in TOPICS {
            assert!(list.contains(&format!("- {name}: {about}\n")), "{list}");
        }
    }

    #[test]
    fn an_unknown_topic_is_one_sentence_listing_the_topics() {
        let e = crate::run(r#"{"help": "joins"}"#).unwrap_err();
        assert_eq!(
            e,
            "help topic \"joins\" is not known; use topics, inputs, sql, csv, output, paging, saving, limits"
        );
    }

    #[test]
    fn every_input_field_is_explained_in_some_topic() {
        let manifest: Value = serde_json::from_str(include_str!("../../../manifest.json")).unwrap();
        let fields = manifest["plugins"][0]["input_schema"]["properties"]
            .as_object()
            .unwrap();
        let missing: Vec<&String> = fields
            .keys()
            .filter(|k| !matches!(k.as_str(), "path_original" | "help"))
            .filter(|k| {
                let quoted = format!("`{k}`");
                !TOPICS.iter().any(|(_, _, text)| text.contains(&quoted))
            })
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
        assert!(fields.contains_key("help"));
    }
}
