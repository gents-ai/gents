use super::*;

fn cc() -> Option<CacheControl> {
    Some(CacheControl { ttl: None })
}

const ATTR: &str = "x-anthropic-billing-header: cc_version=2.1.311;";

#[test]
fn body_system_follows_the_lean_model() {
    gents_lean_vocab::families::claude_prompt_assembly_body_system::replay(|input| {
        let mut strings = vec![input.attribution.as_str(), CLI_IDENTITY];
        strings.extend(input.preamble.as_deref());
        for row in &input.rows {
            strings.extend(row.strip_prefix("system:"));
        }
        let blocks = system_blocks(strings, cc());
        assert!(blocks
            .iter()
            .all(|b| b.cache_control.is_none() || b.cache_control == cc()));
        gents_lean_vocab::families::claude_prompt_assembly_body_system::BodySystem {
            system: blocks.iter().map(|b| b.text.clone()).collect(),
            system_marked: blocks.iter().map(|b| b.cache_control.is_some()).collect(),
        }
    });
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
