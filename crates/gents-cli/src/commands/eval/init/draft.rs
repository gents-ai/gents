//! The author's reply as a draft: exactly one fenced `json` block whose top
//! level is `{"definition": {...}, "cases": [...]}`. A reply without one is
//! an interview turn.

use serde_json::Value;

/// What the author drafted. The CLI assembles the rest of the definition.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Draft {
    pub(crate) definition_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) cases: Vec<Value>,
}

/// `Ok(None)` when the reply carries no fenced json block (an interview turn).
pub(crate) fn parse_reply(reply: &str) -> Result<Option<Draft>, String> {
    let blocks = json_blocks(reply);
    let block = match blocks.as_slice() {
        [] => return Ok(None),
        [block] => block,
        blocks => {
            return Err(format!(
                "the reply has {} fenced json blocks; a draft is exactly one",
                blocks.len()
            ))
        }
    };
    let value: Value = serde_json::from_str(block)
        .map_err(|error| format!("the draft's json block does not parse: {error}"))?;
    let top = value
        .as_object()
        .ok_or("the draft's top level must be an object with definition and cases")?;
    let definition = top
        .get("definition")
        .and_then(Value::as_object)
        .ok_or("the draft needs a \"definition\" object")?;
    let cases = top
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("the draft needs a \"cases\" array")?
        .clone();
    let text = |key: &str| -> Result<Option<String>, String> {
        match definition.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("the draft's definition.{key} must be a string")),
        }
    };
    Ok(Some(Draft {
        definition_id: text("definition_id")?,
        title: text("title")?,
        cases,
    }))
}

/// The bodies of the reply's fenced `json` blocks: a line that is exactly
/// ```` ```json ```` up to the next line that is exactly ```` ``` ````. An
/// unterminated block runs to the end of the reply.
fn json_blocks(reply: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in reply.lines() {
        let fence = line.trim();
        match current.as_mut() {
            None if fence == "```json" => current = Some(Vec::new()),
            None => {}
            Some(_) if fence == "```" => {
                blocks.push(current.take().unwrap_or_default().join("\n"));
            }
            Some(lines) => lines.push(line),
        }
    }
    if let Some(lines) = current {
        blocks.push(lines.join("\n"));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fenced(body: &str) -> String {
        format!("Here is the draft.\n```json\n{body}\n```\n")
    }

    #[test]
    fn prose_alone_is_an_interview_turn() {
        assert_eq!(
            parse_reply("What must the behavior never do?\n```\nnot json\n```"),
            Ok(None)
        );
    }

    #[test]
    fn two_json_blocks_are_refused() {
        let reply = format!("{}{}", fenced("{}"), fenced("{}"));
        let error = parse_reply(&reply).unwrap_err();
        assert!(error.contains("exactly one"), "{error}");
    }

    #[test]
    fn a_block_that_is_not_json_carries_the_parser_message() {
        let error = parse_reply(&fenced("{\"cases\": [")).unwrap_err();
        let serde = serde_json::from_str::<Value>("{\"cases\": [").unwrap_err();
        assert!(error.contains(&serde.to_string()), "{error}");
    }

    #[test]
    fn a_block_without_cases_or_definition_is_refused() {
        let error = parse_reply(&fenced(r#"{"definition": {}}"#)).unwrap_err();
        assert!(error.contains("cases"), "{error}");
        let error = parse_reply(&fenced(r#"{"cases": []}"#)).unwrap_err();
        assert!(error.contains("definition"), "{error}");
    }

    #[test]
    fn a_draft_carries_its_definition_fields_and_cases() {
        let body = json!({
            "definition": {"definition_id": "canary-quality", "title": "Canary"},
            "cases": [{"case_id": "a"}],
        });
        let draft = parse_reply(&fenced(&body.to_string())).unwrap().unwrap();
        assert_eq!(draft.definition_id.as_deref(), Some("canary-quality"));
        assert_eq!(draft.title.as_deref(), Some("Canary"));
        assert_eq!(draft.cases, vec![json!({"case_id": "a"})]);
    }
}
