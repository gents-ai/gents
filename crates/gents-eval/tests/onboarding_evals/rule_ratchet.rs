use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use gents_pack::pack::core_packs::CorePack;
use gents_pack::pack::AGENT_SLOT_REFERENCE_PREFIX;
use gents_protocol::tool_names::{
    AGENT_TOOL_NAMES, META_TOOL_NAMES, NATIVE_TOOL_NAMES, SELF_CONFIG_TOOL_NAMES,
};
use serde_json::Value;

use super::registry::{definition, fixtures, repo, suites, Suite};
use super::rules::{findings, Rule, Subject};

const RULES: &str = "crates/gents/tests/fixtures/configurator_evals/AGENTS.md";
const CELL_AGENT: &str = "engineer";
const ENGINEER_CONTEXT_ID: &str = "engineer-context";
const ENGINEER_TOOLS_ID: &str = "engineer-tools";

#[rustfmt::skip]
const ENGINEER_PIN: &[(&str, usize)] = &[];
#[rustfmt::skip]
const LONG_PROMPT: &[(&str, usize)] = &[];
#[rustfmt::skip]
const PROMPT_NAMES_TOOL: &[(&str, usize)] = &[];
#[rustfmt::skip]
const ROUTE: &[(&str, usize)] = &[];
#[rustfmt::skip]
const UNEXERCISED: &[(&str, usize)] = &[];

const OPEN: [(Rule, &[(&str, usize)]); 5] = [
    (Rule::EngineerPin, ENGINEER_PIN),
    (Rule::LongPrompt, LONG_PROMPT),
    (Rule::PromptNamesTool, PROMPT_NAMES_TOOL),
    (Rule::Route, ROUTE),
    (Rule::Unexercised, UNEXERCISED),
];

#[test]
fn cases_break_the_eval_rules_only_as_open_lists_and_open_only_shrinks() {
    let found = scan();
    let mut open = BTreeMap::new();
    for (rule, cases) in OPEN {
        for (case, count) in cases {
            assert!(
                open.insert((rule, (*case).to_owned()), *count).is_none(),
                "OPEN lists {case} twice under rule {}",
                rule.number()
            );
        }
    }
    let added: Vec<String> = found
        .iter()
        .filter(|(key, details)| details.len() > open.get(*key).copied().unwrap_or(0))
        .flat_map(|((rule, case), details)| {
            details
                .iter()
                .map(move |detail| format!("  {case}: {detail} (rule {})", rule.number()))
        })
        .collect();
    assert!(
        added.is_empty(),
        "these cases break the rules in {RULES} (\"Rules for eval cases\") more often than OPEN in {} allows; change them to keep the rules. A route the prompt itself asks for takes a route_reason on its tool_calls_expected check.\n{}",
        file!(),
        added.join("\n")
    );
    let shrunk: BTreeMap<(Rule, String), usize> = open
        .iter()
        .map(|(key, count)| {
            let now = found.get(key).map_or(0, BTreeSet::len);
            (key.clone(), now.min(*count))
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    assert!(
        shrunk == open,
        "cases break the rules in {RULES} less often than OPEN says; it only shrinks, so replace its five lists in {} with:\n\n{}",
        file!(),
        render(&shrunk)
    );
}

fn scan() -> BTreeMap<(Rule, String), BTreeSet<String>> {
    let suites = suites();
    let tools = known_tools(&suites);
    let mut found: BTreeMap<(Rule, String), BTreeSet<String>> = BTreeMap::new();
    for suite in &suites {
        let subject = subject(tools.clone());
        let paths = case_paths(&suite.dir);
        for case in definition(&suite.dir).cases {
            let path = &paths[&case.case_id];
            for (rule, detail) in findings(&case, &subject) {
                found
                    .entry((rule, path.clone()))
                    .or_default()
                    .insert(detail);
            }
        }
    }
    found
}

fn subject(tools: BTreeSet<String>) -> Subject {
    Subject {
        engineer: format!("{AGENT_SLOT_REFERENCE_PREFIX}{CELL_AGENT}"),
        tools_id: ENGINEER_TOOLS_ID.to_owned(),
        context_id: ENGINEER_CONTEXT_ID.to_owned(),
        tools,
    }
}

fn known_tools(suites: &[Suite]) -> BTreeSet<String> {
    let mut tools: BTreeSet<String> = NATIVE_TOOL_NAMES
        .iter()
        .chain(&AGENT_TOOL_NAMES)
        .chain(&META_TOOL_NAMES)
        .chain(&SELF_CONFIG_TOOL_NAMES)
        .map(|name| (*name).to_owned())
        .collect();
    for pack in CorePack::ALL {
        let (_, name) = pack.coordinate().split_once('/').unwrap();
        let manifest = json(&repo().join("core-packs").join(name).join("manifest.json"));
        tools.extend(
            array(&manifest["plugins"])
                .filter_map(|plugin| plugin["name"].as_str().map(str::to_owned)),
        );
    }
    for suite in suites {
        for case in definition(&suite.dir).cases {
            for check in case
                .stages
                .iter()
                .flat_map(|stage| &stage.checks)
                .filter(|check| check.check == "tool_calls_expected")
            {
                let params = &check.params;
                tools.extend(
                    array(&params["required"])
                        .chain(array(&params["forbidden"]))
                        .filter_map(|tool| tool.as_str().map(str::to_owned)),
                );
                tools.extend(
                    params["allowed_argv"]
                        .as_object()
                        .into_iter()
                        .flat_map(|argv| argv.keys().cloned()),
                );
            }
        }
    }
    tools
}

fn case_paths(dir: &Path) -> BTreeMap<String, String> {
    let config = json(&dir.join("pack_config.json"));
    array(&config["eval_definitions"][0]["cases"])
        .map(|file| {
            let file = dir.join(file.as_str().unwrap().trim_start_matches("./"));
            let case_id = json(&file)["case_id"].as_str().unwrap().to_owned();
            let relative = file.strip_prefix(fixtures()).unwrap();
            (case_id, relative.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

fn render(open: &BTreeMap<(Rule, String), usize>) -> String {
    OPEN.iter()
        .map(|(rule, _)| {
            let name = const_name(*rule);
            let lines: Vec<String> = open
                .iter()
                .filter(|((listed, _), _)| listed == rule)
                .map(|((_, case), count)| format!("    ({case:?}, {count}),"))
                .collect();
            if lines.is_empty() {
                format!("#[rustfmt::skip]\nconst {name}: &[(&str, usize)] = &[];")
            } else {
                format!(
                    "#[rustfmt::skip]\nconst {name}: &[(&str, usize)] = &[\n{}\n];",
                    lines.join("\n")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn const_name(rule: Rule) -> String {
    let mut name = String::new();
    for (index, c) in format!("{rule:?}").chars().enumerate() {
        if c.is_uppercase() && index > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    name
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn array(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}
