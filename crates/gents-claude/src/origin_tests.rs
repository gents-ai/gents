use super::*;
use crate::system::identity_for;

fn cause(name: &str) -> Cause {
    match name {
        "client" => Cause::Client,
        "agent_new" => Cause::AgentNew,
        "task_trigger" => Cause::TaskTrigger,
        "plugin_job" => Cause::PluginJob,
        other => panic!("unknown cause {other}"),
    }
}

fn kind(name: &str) -> Kind {
    match name {
        "turn" => Kind::Turn,
        "compaction" => Kind::Compaction,
        "title" => Kind::Title,
        "one_shot" => Kind::OneShot,
        other => panic!("unknown kind {other}"),
    }
}

fn flag(case: &serde_json::Value, key: &str) -> bool {
    case[key].as_bool().unwrap()
}

#[test]
fn generated_origin_cases_pick_the_wire_mode() {
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["prompt_assembly_claude_origin_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 128, "the Lean cases enumerate every origin");
    for case in cases {
        let name = &case["name"];
        let mode = Origin {
            cause: cause(case["cause"].as_str().unwrap()),
            root_client: flag(case, "root_client"),
            scheduled: flag(case, "scheduled"),
            kind: kind(case["kind"].as_str().unwrap()),
            appends_system_prompt: flag(case, "appends_system_prompt"),
        }
        .wire_mode();
        assert_eq!(
            identity_for(false, mode.session),
            case["identity"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            mode.entrypoint,
            case["entrypoint"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(mode.background, flag(case, "background"), "{name}");
        assert_eq!(mode.workload, case["workload"].as_str(), "{name}");
        assert_eq!(mode.subagent, flag(case, "subagent"), "{name}");
        assert_eq!(
            mode.request_class.as_str(),
            case["request_class"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            mode.main_thread_source,
            flag(case, "main_thread_source"),
            "{name}"
        );
        assert_eq!(mode.agentic, flag(case, "agentic"), "{name}");
    }
}

#[test]
fn a_scheduled_task_is_a_background_print_job() {
    let mode = Origin {
        cause: Cause::TaskTrigger,
        root_client: false,
        scheduled: true,
        kind: Kind::Turn,
        appends_system_prompt: true,
    }
    .wire_mode();
    assert_eq!(mode.entrypoint, PRINT_ENTRYPOINT);
    assert!(mode.background);
    assert_eq!(mode.workload, Some(CRON_WORKLOAD));
    assert_eq!(mode.request_class, RequestClass::Main);
    assert!(mode.main_thread_source);
}

#[test]
fn a_subagent_of_a_chat_stays_interactive_without_the_one_hour_source() {
    let mode = Origin {
        cause: Cause::AgentNew,
        root_client: true,
        scheduled: false,
        kind: Kind::Turn,
        appends_system_prompt: true,
    }
    .wire_mode();
    assert_eq!(mode.session, SessionMode::Interactive);
    assert!(mode.subagent && mode.agentic);
    assert!(!mode.main_thread_source);
}
