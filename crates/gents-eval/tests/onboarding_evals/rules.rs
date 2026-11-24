use std::collections::BTreeSet;

use gents_documents::{EvalCapture, EvalCase, EvalCheckRef, EvalStage};
use gents_store::collection::Collection;
use serde_json::Value;

const MAX_PROMPT_CHARS: usize = 300;
const CALLING_WORDS: [&str; 12] = [
    "call", "calls", "calling", "use", "uses", "using", "via", "with", "through", "invoke",
    "invokes", "invoking",
];
const TOOL_WORDS: [&str; 4] = ["tool", "tools", "call", "calls"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Rule {
    EngineerPin,
    LongPrompt,
    PromptNamesTool,
    Route,
    Unexercised,
}

impl Rule {
    pub(super) fn number(self) -> u8 {
        match self {
            Self::EngineerPin => 1,
            Self::LongPrompt | Self::PromptNamesTool => 2,
            Self::Route => 3,
            Self::Unexercised => 4,
        }
    }
}

pub(super) struct Subject {
    pub(super) engineer: String,
    pub(super) context_id: String,
    pub(super) tools_id: String,
    pub(super) tools: BTreeSet<String>,
}

pub(super) fn findings(case: &EvalCase, subject: &Subject) -> BTreeSet<(Rule, String)> {
    let mut found = BTreeSet::new();
    for stage in &case.stages {
        let mut add = |rule: Rule, detail: String| {
            found.insert((rule, format!("stage {}: {detail}", stage.stage_id)));
        };
        for check in &stage.checks {
            for field in engineer_pins(check, subject) {
                add(
                    Rule::EngineerPin,
                    format!(
                        "{} pins the Engineer's {field} to an exact value",
                        check.check
                    ),
                );
            }
            for route in routes(check) {
                add(Rule::Route, route);
            }
        }
        let continuation = stage
            .continuation
            .as_ref()
            .map(|continuation| ("continuation prompt", continuation.prompt.as_str()));
        for (what, prompt) in std::iter::once(("prompt", stage.prompt.as_str())).chain(continuation)
        {
            if prompt.chars().count() > MAX_PROMPT_CHARS {
                add(
                    Rule::LongPrompt,
                    format!("{what} over {MAX_PROMPT_CHARS} characters"),
                );
            }
            for tool in subject.tools.iter().filter(|tool| names_tool(prompt, tool)) {
                add(
                    Rule::PromptNamesTool,
                    format!("{what} names the tool {tool}"),
                );
            }
        }
    }
    if let Some(stage) = unexercised(case) {
        found.insert((
            Rule::Unexercised,
            format!(
                "stage {}: builds and no later stage exercises it",
                stage.stage_id
            ),
        ));
    }
    found
}

fn engineer_pins(check: &EvalCheckRef, subject: &Subject) -> BTreeSet<&'static str> {
    let mut pinned = BTreeSet::new();
    if check.check != "crew_spec_match" {
        return pinned;
    }
    let params = &check.params;
    for spec in array(&params["agents"]["expect"])
        .filter(|spec| spec["agent_did"].as_str() == Some(subject.engineer.as_str()))
    {
        pinned.extend(exact_fields(&spec["context"]).chain(exact_fields(&spec["tools"])));
    }
    for row in array(&params["rows"]).chain(array(&params["links"])) {
        let id = row["id"].as_str();
        let engineer = match row["key"].as_str() {
            Some("context_id") => id == Some(subject.context_id.as_str()),
            Some("tools_id") => id == Some(subject.tools_id.as_str()),
            _ => false,
        };
        if engineer {
            pinned.extend(exact_fields(&row["expect"]));
        }
    }
    pinned
}

fn exact_fields(expectations: &Value) -> impl Iterator<Item = &'static str> + '_ {
    array(expectations)
        .filter(|expectation| expectation.get("equals").is_some())
        .filter_map(|expectation| match expectation["field"].as_str() {
            Some("system_prompt") => Some("system_prompt"),
            Some("self_config.self_config_categories") => Some("self_config_categories"),
            _ => None,
        })
}

fn names_tool(prompt: &str, tool: &str) -> bool {
    let identifier = |c: char| c.is_alphanumeric() || c == '_';
    prompt.match_indices(tool).any(|(start, _)| {
        let before = &prompt[..start];
        let after = &prompt[start + tool.len()..];
        let mut next = after.chars();
        let in_path = match next.next() {
            Some('/' | '-') => true,
            Some('.') => next.next().is_some_and(char::is_alphanumeric),
            _ => false,
        } || before.ends_with(['/', '-']);
        if in_path
            || before.chars().next_back().is_some_and(identifier)
            || after.chars().next().is_some_and(identifier)
        {
            return false;
        }
        before.ends_with('`')
            || first_word(after).is_some_and(|word| TOOL_WORDS.contains(&word.as_str()))
            || last_word(before).is_some_and(|word| CALLING_WORDS.contains(&word.as_str()))
            || tool.contains('_')
    })
}

fn first_word(text: &str) -> Option<String> {
    let rest = text.strip_prefix(char::is_whitespace)?.trim_start();
    let word: String = rest.chars().take_while(|c| c.is_alphabetic()).collect();
    (!word.is_empty()).then(|| word.to_lowercase())
}

fn last_word(text: &str) -> Option<String> {
    let rest = text.strip_suffix(char::is_whitespace)?.trim_end();
    let start = rest
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic())
        .last()
        .map(|(index, _)| index)?;
    Some(rest[start..].to_lowercase())
}

fn routes(check: &EvalCheckRef) -> Vec<String> {
    let params = &check.params;
    let reasoned = params["route_reason"]
        .as_str()
        .is_some_and(|reason| !reason.trim().is_empty());
    if check.check != "tool_calls_expected" || reasoned {
        return Vec::new();
    }
    let mut routes: Vec<String> = array(&params["required"])
        .filter_map(Value::as_str)
        .map(|tool| format!("requires {tool}"))
        .chain(
            array(&params["forbidden"])
                .filter_map(Value::as_str)
                .map(|tool| format!("forbids {tool}")),
        )
        .chain(
            params["allowed_argv"]
                .as_object()
                .into_iter()
                .flat_map(|argv| argv.keys())
                .map(|tool| format!("limits {tool} arguments with allowed_argv")),
        )
        .collect();
    if !params["max_calls"].is_null() {
        routes.push("caps its tool calls with max_calls".into());
    }
    routes
}

fn unexercised(case: &EvalCase) -> Option<&EvalStage> {
    let built = case.stages.iter().position(|stage| {
        stage
            .checks
            .iter()
            .any(|check| grades_configuration(stage, check))
    })?;
    let stage = &case.stages[built];
    let exercised = stage
        .checks
        .iter()
        .any(|check| grades_a_run_in_place(stage, check))
        || case.stages[built + 1..]
            .iter()
            .any(|stage| stage.checks.iter().any(|check| grades_a_run(stage, check)));
    (!exercised).then_some(stage)
}

fn grades_a_run_in_place(stage: &EvalStage, check: &EvalCheckRef) -> bool {
    !matches!(
        check.check.as_str(),
        "final_message_matches" | "captured_files_match"
    ) && !grades_absence(check)
        && grades_a_run(stage, check)
}

fn grades_configuration(stage: &EvalStage, check: &EvalCheckRef) -> bool {
    match check.check.as_str() {
        "schema_matches" => true,
        "crew_spec_match" => {
            !grades_absence(check) && read_captures(stage, check).any(is_configuration)
        }
        _ => false,
    }
}

fn grades_absence(check: &EvalCheckRef) -> bool {
    let params = &check.params;
    match check.check.as_str() {
        "crew_spec_match" => {
            params
                .as_object()
                .is_some_and(|object| object.keys().all(|key| key == "present"))
                && array(&params["present"]).all(|entry| entry["max"] == 0)
        }
        "captured_rows_count" => params["max"] == 0,
        _ => false,
    }
}

fn grades_a_run(stage: &EvalStage, check: &EvalCheckRef) -> bool {
    match check.check.as_str() {
        "final_message_matches"
        | "tool_result_matches"
        | "handoff_delivery"
        | "captured_files_match" => true,
        "captured_rows_count" | "captured_fields_match" | "crew_spec_match" => {
            read_captures(stage, check).any(|capture| {
                matches!(capture, EvalCapture::Documents { .. }) && !is_configuration(capture)
            })
        }
        _ => false,
    }
}

fn read_captures<'a>(
    stage: &'a EvalStage,
    check: &EvalCheckRef,
) -> impl Iterator<Item = &'a EvalCapture> {
    let named: BTreeSet<String> = strings(&check.params).map(str::to_owned).collect();
    stage
        .capture
        .iter()
        .filter(move |capture| named.contains(capture.name()))
}

fn is_configuration(capture: &EvalCapture) -> bool {
    match capture {
        EvalCapture::Schema { .. } => true,
        EvalCapture::Documents { collection, .. } => Collection::ALL
            .iter()
            .any(|known| known.graphql_type() == collection),
        EvalCapture::File { .. } => false,
    }
}

fn array(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn strings(value: &Value) -> Box<dyn Iterator<Item = &str> + '_> {
    match value {
        Value::String(text) => Box::new(std::iter::once(text.as_str())),
        Value::Array(items) => Box::new(items.iter().flat_map(strings)),
        Value::Object(object) => Box::new(object.values().flat_map(strings)),
        _ => Box::new(std::iter::empty()),
    }
}
