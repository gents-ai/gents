use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Draft {
    pub(crate) definition_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) cases: Vec<Value>,
}

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

pub(crate) fn json_blocks(reply: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in reply.lines() {
        let fence = line.trim();
        match current.as_mut() {
            None if is_json_fence(fence) => current = Some(Vec::new()),
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

fn is_json_fence(fence: &str) -> bool {
    fence
        .strip_prefix("```")
        .is_some_and(|info| info.trim().eq_ignore_ascii_case("json"))
}

