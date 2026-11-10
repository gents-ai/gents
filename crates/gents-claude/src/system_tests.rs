use serde_json::Value;

use super::*;

fn cc() -> Option<CacheControl> {
    Some(CacheControl { ttl: None })
}

const ATTR: &str = "x-anthropic-billing-header: cc_version=2.1.311;";

#[test]
fn generated_body_cases_split_the_system() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["prompt_assembly_claude_body_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let mut strings = vec![case["attribution"].as_str().unwrap(), CLI_IDENTITY];
        strings.extend(case["preamble"].as_str());
        for row in case["rows"].as_array().unwrap() {
            strings.extend(row.as_str().unwrap().strip_prefix("system:"));
        }
        let blocks = system_blocks(strings, cc());
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        let want: Vec<&str> = case["system"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let marked: Vec<bool> = blocks.iter().map(|b| b.cache_control.is_some()).collect();
        let want_marked: Vec<bool> = case["system_marked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_bool().unwrap())
            .collect();
        assert_eq!(texts, want, "{}", case["name"]);
        assert_eq!(marked, want_marked, "{}", case["name"]);
        assert!(blocks
            .iter()
            .all(|b| b.cache_control.is_none() || b.cache_control == cc()));
    }
}

#[test]
fn identity_choice() {
    let ni = |appends_system_prompt| SessionMode::NonInteractive {
        appends_system_prompt,
    };
    assert_eq!(identity_for(false, SessionMode::Interactive), CLI_IDENTITY);
    assert_eq!(identity_for(true, ni(false)), CLI_IDENTITY);
    assert_eq!(identity_for(true, ni(true)), CLI_IDENTITY);
    assert_eq!(identity_for(false, ni(true)), SDK_IDENTITY);
    assert_eq!(identity_for(false, ni(false)), AGENT_IDENTITY);
}

#[test]
fn last_attribution_and_identity_win() {
    let blocks = system_blocks(
        [
            ATTR,
            "x-anthropic-billing-header: second",
            CLI_IDENTITY,
            SDK_IDENTITY,
            "p",
        ],
        cc(),
    );
    let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(
        texts,
        ["x-anthropic-billing-header: second", SDK_IDENTITY, "p"]
    );
    assert_eq!(blocks[0].cache_control, None);
    assert_eq!(blocks[1].cache_control, cc());
}

#[test]
fn empty_strings_drop_and_prompt_joins() {
    let blocks = system_blocks([ATTR, CLI_IDENTITY, "", "one", "", "two"], cc());
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[2].text, "one\n\ntwo");
    assert_eq!(system_blocks(["", ""], cc()), vec![]);
    assert_eq!(system_blocks([ATTR, CLI_IDENTITY, ""], cc()).len(), 2);
}

#[test]
fn caching_off_marks_nothing() {
    let blocks = system_blocks([ATTR, CLI_IDENTITY, "p"], None);
    assert!(blocks.iter().all(|b| b.cache_control.is_none()));
}
