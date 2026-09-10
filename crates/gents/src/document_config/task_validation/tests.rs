use super::*;
use serde_json::json;

#[test]
fn task_templates_and_goal_settings_keep_existing_authoring_guards() {
    let base = json!({"agent_did":"owner", "task_id":"task", "behavior_id":"behavior",
        "prompt_template":"{{ node.node_did }} {{ node.behavior_id }} {{ ctx.now }} {{ doc.name }}"});
    let decode = |value| serde_json::from_value::<Task>(value).unwrap();
    decode(base.clone()).validate().unwrap();
    let mut goal = base.clone();
    goal["goal_objective_template"] = json!("Finish {{ doc.name }}");
    goal["goal_token_budget"] = json!(1000);
    decode(goal.clone()).validate().unwrap();
    for field in ["prompt_template", "goal_objective_template"] {
        for text in ["{{ ctx.not_available }}", "{{ node.api_key }}"] {
            let mut invalid = goal.clone();
            invalid[field] = json!(text);
            assert!(
                decode(invalid).validate().is_err(),
                "accepted {field}={text}"
            );
        }
    }
    for field in ["prompt_template", "goal_objective_template"] {
        let mut oversized = goal.clone();
        oversized[field] = json!("x".repeat(65 * 1024));
        assert!(decode(oversized).validate().is_err());
    }
    for budget in [0, -1] {
        let mut invalid = goal.clone();
        invalid["goal_token_budget"] = json!(budget);
        assert!(decode(invalid).validate().is_err());
    }
    let mut missing_objective = base;
    missing_objective["goal_token_budget"] = json!(1000);
    assert!(decode(missing_objective).validate().is_err());
    goal["goal_objective_template"] = json!(" ");
    assert!(decode(goal).validate().is_err());
}

#[test]
fn task_hook_admission_matches_existing_lean_guards() {
    let task = |hooks| {
        serde_json::from_value::<Task>(json!({
            "agent_did":"owner", "task_id":"task", "behavior_id":"behavior",
            "prompt_template":"literal", "hooks":hooks
        }))
        .unwrap()
    };
    let hook = json!({"hook_id":"prepare", "phase":"before", "command":["printf", ""]});
    task(json!([])).validate().unwrap();
    task(json!([hook.clone()])).validate().unwrap();
    for timeout in [1, 120, i64::MAX] {
        let mut valid = hook.clone();
        valid["timeout_secs"] = json!(timeout);
        task(json!([valid])).validate().unwrap();
    }
    for timeout in [0, -1, i64::MIN] {
        let mut invalid = hook.clone();
        invalid["timeout_secs"] = json!(timeout);
        assert!(task(json!([invalid])).validate().is_err());
    }
    let mut empty = hook.clone();
    empty["command"] = json!([]);
    assert!(task(json!([empty])).validate().is_err());
    let mut cleanup = hook.clone();
    cleanup["phase"] = json!("finally");
    assert!(task(json!([hook.clone(), cleanup.clone()]))
        .validate()
        .is_err());
    cleanup["hook_id"] = json!(" prepare ");
    task(json!([hook, cleanup])).validate().unwrap();
    // Admission does not invent command parsing: execution reports an invalid host executable.
    task(json!([{"hook_id":"", "phase":"before", "command":[""]}]))
        .validate()
        .unwrap();
}
