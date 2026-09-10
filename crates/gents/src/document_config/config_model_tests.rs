use super::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

fn round_trip<T: DeserializeOwned + Serialize>(authored: Value) -> T {
    let parsed: T = serde_json::from_value(authored.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), authored);
    parsed
}

#[test]
fn task_hooks_are_compact_explicit_and_preserve_order() {
    let authored = json!({"task_id":"code", "agent_did":"did:key:test", "behavior_id":"coder",
    "prompt_template":"Do {{args.work}}", "hooks":[
        {"hook_id":"prepare", "command":["./prepare.sh"], "phase":"before"},
        {"hook_id":"verify", "command":["./verify.sh"], "phase":"after_success"},
        {"hook_id":"cleanup", "command":["./cleanup.sh"], "phase":"finally"}
    ]});
    let task = round_trip::<Task>(authored);
    assert!(task.enabled);
    assert_eq!(task.hooks[2].phase, TaskHookPhase::Finally);
    assert!(serde_json::from_value::<TaskHook>(
        json!({"hook_id":"x", "command":["./hook.sh"], "phase":"after"})
    )
    .is_err());
}

#[test]
fn reusable_callback_and_event_binding_are_distinct() {
    round_trip::<Callback>(json!({"callback_id":"prepare", "agent_did":"did:key:test",
        "handler":{"kind":"built_in", "emitter":"create_workspace"}, "capabilities":["create_workspace"]}));
    round_trip::<CallbackBinding>(json!({"binding_id":"on-work", "agent_did":"did:key:test",
        "event_source_id":"work", "callback_id":"prepare"}));
    round_trip::<CallbackInvocationOrigin>(json!({"kind":"event_group",
        "binding_id":"on-work", "group_key":"group-1"}));
}

#[test]
fn null_defaults_are_compact_but_explicit_disable_and_tags_survive() {
    let task: Task = serde_json::from_value(json!({"task_id":"code", "agent_did":"did:key:test",
        "behavior_id":"coder", "prompt_template":"work", "enabled":null,"hooks":null,"tags":null}))
    .unwrap();
    assert!(task.enabled);
    assert!(task.hooks.is_empty());
    assert!(task.tags.is_empty());
    assert_eq!(
        serde_json::to_value(task).unwrap(),
        json!({"task_id":"code", "agent_did":"did:key:test",
        "behavior_id":"coder", "prompt_template":"work"})
    );
    round_trip::<AgentContext>(
        json!({"context_id":"code", "agent_did":"did:key:test", "tags":["coding"]}),
    );
    round_trip::<AgentBehavior>(json!({"behavior_id":"coder", "agent_did":"did:key:test",
        "inference_profile_id":"fast", "enabled":false, "tags":["coding"]}));
}

#[test]
fn required_scope_and_inference_selection_do_not_default() {
    assert!(serde_json::from_value::<Task>(
        json!({"task_id":"code", "behavior_id":"coder", "prompt_template":"work"})
    )
    .is_err());
    assert!(serde_json::from_value::<AgentBehavior>(
        json!({"behavior_id":"coder", "agent_did":"did:key:test"})
    )
    .is_err());
    assert!(serde_json::from_value::<InferenceProfile>(
        json!({"profile_id":"fast", "backend_id":"api", "model_name":"model"})
    )
    .is_err());
    round_trip::<InferenceProfile>(
        json!({"profile_id":"fast", "agent_did":"did:key:test", "backend_id":"api", "model_name":"model"}),
    );
}

#[test]
fn config_rejects_typos_wrong_tag_types_and_removed_fields() {
    assert!(
        serde_json::from_value::<Task>(json!({"task_id":"code", "agent_did":"did:key:test",
        "behavior_id":"coder", "prompt_template":"work", "hook":[]}))
        .is_err()
    );
    assert!(serde_json::from_value::<AgentContext>(
        json!({"context_id":"code", "agent_did":"did:key:test", "tags":[7]})
    )
    .is_err());
    assert!(serde_json::from_value::<AgentBehavior>(
        json!({"behavior_id":"coder", "agent_did":"did:key:test",
        "inference_profile_id":"fast", "model_name":"old-override"})
    )
    .is_err());
    assert!(serde_json::from_value::<Tools>(
        json!({"tools_id":"code", "agent_did":"did:key:test", "host":{"bashh":{}}})
    )
    .is_err());
}

#[test]
fn storage_identity_is_unwrapped_before_strict_config_deserialization() {
    let data = json!({"Task":[{"_docID":"stored-id", "task_id":"code", "agent_did":"did:key:test",
        "behavior_id":"coder", "prompt_template":"work"}]});
    let rows = serde_helpers::rows_with_doc_id::<Task>(Some(&data), "Task");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "stored-id");
    assert_eq!(rows[0].1.task_id, "code");
}

#[test]
fn callback_module_and_local_installation_config_share_compact_metadata() {
    round_trip::<CallbackModule>(
        json!({"module_id":"verify", "agent_did":"did:key:test", "tags":["coding"]}),
    );
    round_trip::<RepositoryPlacement>(
        json!({"repository_id":"repo", "agent_did":"did:key:test", "host_path":"/workspace"}),
    );
    round_trip::<ToolServiceRegistry>(
        json!({"service_id":"web", "agent_did":"did:key:test", "hostname":"localhost", "mcp_port":9000}),
    );
}

#[test]
fn graph_delivery_uses_the_same_grouping_and_concurrency_types() {
    let expected: crate::graph_pipeline::GroupCount = EventGroupCount::Fixed(2);
    let group = EventGroup {
        expected_count: Some(expected),
        timeout_secs: Some(30),
        min_count: None,
    };
    let delivery: crate::graph_pipeline::DeliveryMode = Some(group);
    let concurrency: crate::graph_pipeline::DeliveryConcurrency = ConcurrencyMode::Serial;
    assert_eq!(
        serde_json::to_value(delivery).unwrap(),
        json!({"expected_count":2,"timeout_secs":30})
    );
    assert_eq!(serde_json::to_value(concurrency).unwrap(), json!("serial"));
}

#[test]
fn context_skill_selection_is_compact_and_system_instructions_stay_literal() {
    let minimal = json!({"context_id":"coding", "agent_did":"did:key:test"});
    for skill_ids in [Value::Null, json!([])] {
        let mut authored = minimal.clone();
        authored["skill_ids"] = skill_ids;
        let context: AgentContext = serde_json::from_value(authored).unwrap();
        assert!(context.skill_ids.is_empty());
        assert_eq!(serde_json::to_value(context).unwrap(), minimal);
    }

    let context = round_trip::<AgentContext>(json!({
        "context_id":"coding", "agent_did":"did:key:test",
        "system_prompt":"Keep {{ args.target }} and {{ ctx.now }} literal.",
        "skill_ids":["review", "investigate"], "tags":["coding"]
    }));
    assert_eq!(context.skill_ids, ["review", "investigate"]);
    assert_eq!(
        context.system_prompt.as_deref(),
        Some("Keep {{ args.target }} and {{ ctx.now }} literal.")
    );
}

#[test]
fn execution_and_retry_keep_explicit_overrides_distinct_from_omission() {
    let execution = round_trip::<InferenceExecution>(
        json!({"execution_id":"coding", "agent_did":"did:key:test"}),
    );
    assert_eq!(execution.stream_batch_ms, None);
    assert_eq!(execution.retry_policy_id, None);
    let retry = round_trip::<InferenceRetryPolicy>(
        json!({"retry_policy_id":"coding", "agent_did":"did:key:test"}),
    );
    assert_eq!(retry.max_transport_retries, None);
    assert_eq!(retry.allow_repair, None);
    assert_eq!(retry.backoff_ms, None);

    let execution = round_trip::<InferenceExecution>(json!({
        "execution_id":"coding", "agent_did":"did:key:test",
        "stream_batch_ms":25, "stream_liveness_timeout_secs":90,
        "retry_policy_id":"no-repair", "tags":["interactive"]
    }));
    assert_eq!(execution.stream_batch_ms, Some(25));
    let retry = round_trip::<InferenceRetryPolicy>(json!({
        "retry_policy_id":"no-repair", "agent_did":"did:key:test",
        "max_transport_retries":0, "allow_repair":false,
        "backoff_ms":[0, 100], "tags":["interactive"]
    }));
    assert_eq!(retry.max_transport_retries, Some(0));
    assert_eq!(retry.allow_repair, Some(false));
    assert_eq!(retry.backoff_ms.as_deref(), Some([0, 100].as_slice()));
}
