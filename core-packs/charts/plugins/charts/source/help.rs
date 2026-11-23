//! Help mode: a request with `help` names a topic and gets its text back
//! instead of a chart, so the tool description stays a small core and the
//! detail is read only when it is needed.

use serde_json::json;

use crate::err::{ChartError, Res};

/// Each topic: its name, what it covers, and its text.
pub const TOPICS: &[(&str, &str, &str)] = &[
    (
        "inputs",
        "the data (data, path, file) and the columns to draw",
        include_str!("../help/inputs.md"),
    ),
    (
        "types",
        "every chart type and what it draws",
        include_str!("../help/types.md"),
    ),
    (
        "options",
        "titles, labels, axes, ranges, legend, theme, colours and size",
        include_str!("../help/options.md"),
    ),
    (
        "formats",
        "number formats and date axes",
        include_str!("../help/formats.md"),
    ),
    (
        "output",
        "the result, its series, warnings and errors",
        include_str!("../help/output.md"),
    ),
    (
        "saving",
        "writing the chart as SVG and PNG files",
        include_str!("../help/saving.md"),
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
            ChartError::new(format!(
                "help topic {} is not known; use topics, {}",
                crate::text::quote(topic),
                names.join(", ")
            ))
        })
}

/// The tool reply for a help request.
pub fn answer(topic: &str) -> Res<String> {
    Ok(json!({"response": {"help": topic.trim(), "text": text(topic)?}}).to_string())
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
            assert_eq!(v["response"]["help"], *name);
            assert_eq!(v["response"]["text"], *text);
            assert!(text.len() > 200, "{name}");
            assert!(v.get("parts").is_none());
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
    fn help_wins_over_the_other_fields() {
        let v = reply(r#"{"help": "types", "chart": "bar", "data": "k,v\na,1\n"}"#);
        assert_eq!(v["response"]["help"], "types");
    }

    #[test]
    fn an_unknown_topic_is_one_sentence_listing_the_topics() {
        let e = crate::run(r#"{"help": "colours"}"#).unwrap_err();
        assert_eq!(
            e.0,
            "help topic \"colours\" is not known; use topics, inputs, types, options, formats, output, saving"
        );
    }

    #[test]
    fn a_call_without_chart_or_help_names_the_fix() {
        let e = crate::run(r#"{"data": "k,v\na,1\n"}"#).unwrap_err();
        assert!(e.0.starts_with("chart is required; use line, "), "{}", e.0);
        assert!(!e.0.contains('\n'));
    }

    #[test]
    fn an_unknown_field_points_at_the_help() {
        let e = crate::run(r#"{"chart": "bar", "titel": "x"}"#).unwrap_err();
        assert!(e.0.contains("{\"help\": \"inputs\"}"), "{}", e.0);
    }

    #[test]
    fn every_input_field_is_explained_in_some_topic() {
        let manifest: Value = serde_json::from_str(include_str!("../../../manifest.json")).unwrap();
        let schema = &manifest["plugins"][0]["input_schema"];
        let fields = schema["properties"].as_object().unwrap();
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
        assert_eq!(schema["required"], Value::Null);
    }
}
