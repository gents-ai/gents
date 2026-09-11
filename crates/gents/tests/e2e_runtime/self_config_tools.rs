use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess, DesiredStateApplyPlan,
};
use gents::document_config::PackConfig;
use gents::self_config::build_self_config_tools;
use gents::tool_surface::SelfConfigToolConfig;
use gents::Collection;
use serde_json::{json, Value};

use crate::support::test_db;

const AGENT_DID: &str = "did:key:zSelfConfigE2E";
const BEHAVIOR_ID: &str = "self-config-behavior";
const CONTEXT_ID: &str = "self-config-context";
const TOOLS_ID: &str = "self-config-tools";
const PROFILE_ID: &str = "self-config-profile";
const BACKEND_ID: &str = "self-config-backend";
const SECRET: &str = "sk-secret-should-never-leak";

async fn seed_config(node: &Arc<EmbeddedNode>) {
    let config: PackConfig = serde_json::from_value(json!({
        "agent_principal":{"agent_did":AGENT_DID,"default_behavior_id":BEHAVIOR_ID},
        "agent_behaviors":[{"agent_did":AGENT_DID,"behavior_id":BEHAVIOR_ID,"context_id":CONTEXT_ID,"inference_profile_id":PROFILE_ID}],
        "contexts":[{"agent_did":AGENT_DID,"context_id":CONTEXT_ID,"system_prompt":"original prompt","tools_id":TOOLS_ID}],
        "tools":[{"agent_did":AGENT_DID,"tools_id":TOOLS_ID,"self_config":{"enable_self_config":true}}],
        "inference_profiles":[{"agent_did":AGENT_DID,"profile_id":PROFILE_ID,"backend_id":BACKEND_ID,"model_name":"model-small","sampling_id":"sampling","execution_id":"execution"}],
        "inference_sampling":[{"agent_did":AGENT_DID,"sampling_id":"sampling","temperature":0.7}],
        "inference_execution":[{"agent_did":AGENT_DID,"execution_id":"execution","max_turns":40}],
        "inference_backends":[{"agent_did":AGENT_DID,"backend_id":BACKEND_ID,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:11434/v1","auth":{"kind":"api_key","key":SECRET},"max_concurrent":1}]
    })).unwrap();
    let plan = DesiredStateApplyPlan::from_pack_config(&config).unwrap();
    ConfigAccess::Local(node.clone())
        .transact("test.self_config.seed", |txn| {
            let plan = plan.clone();
            Box::pin(async move { apply_desired_state_plan(txn, &plan).await })
        })
        .await
        .unwrap();
}

async fn read(node: &Arc<EmbeddedNode>, collection: Collection, id: &str) -> Value {
    let id = id.to_owned();
    ConfigAccess::Local(node.clone())
        .transact("test.self_config.read", |txn| {
            let id = id.clone();
            Box::pin(async move {
                Ok(
                    read_desired_state_record_in_txn(txn, collection, AGENT_DID, &id)
                        .await?
                        .unwrap()
                        .1,
                )
            })
        })
        .await
        .unwrap()
}

fn tool_config(categories: &[&str], no_lockout: bool, dry_run: bool) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        behavior_id: BEHAVIOR_ID.into(),
        categories: categories.iter().map(|s| s.to_string()).collect(),
        no_lockout,
        dry_run,
    }
}

async fn call_tool(
    tools: &[Box<dyn gents::llm::tool::ToolDyn>],
    name: &str,
    args: Value,
) -> Result<String, String> {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("missing tool {name}"))
        .call(args.to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tokio::test]
async fn configure_context_and_profile_preserve_identity_and_reject_partial_commits() {
    let db = test_db("self-config-bindings").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["behavior", "profile"], false, false),
    );
    call_tool(
        &tools,
        "configure_behavior",
        json!({"target":"context","patch":{"system_prompt":"sharper prompt"}}),
    )
    .await
    .unwrap();
    call_tool(
        &tools,
        "configure_profile",
        json!({"patch":{"model_name":"model-large"}}),
    )
    .await
    .unwrap();
    assert_eq!(
        read(&db.node, Collection::AgentContext, CONTEXT_ID).await["system_prompt"],
        "sharper prompt"
    );
    let before = read(&db.node, Collection::InferenceProfile, PROFILE_ID).await;
    assert_eq!(before["model_name"], "model-large");
    for patch in [
        json!({"agent_did":"did:key:zAttacker","model_name":"hijacked"}),
        json!({"backend_id":"missing-backend","model_name":"half applied?"}),
    ] {
        call_tool(&tools, "configure_profile", json!({"patch":patch}))
            .await
            .expect_err("identity/reference validation must reject the entire patch");
        assert_eq!(
            read(&db.node, Collection::InferenceProfile, PROFILE_ID).await,
            before
        );
    }
    assert_eq!(
        read(&db.node, Collection::AgentBehavior, BEHAVIOR_ID).await["agent_did"],
        AGENT_DID
    );
}

#[tokio::test]
async fn configure_tools_respects_gate_and_no_lockout() {
    let db = test_db("self-config-tools").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["tools"], true, false),
    );
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"built_ins":{"enable_context_budget":true}}}),
    )
    .await
    .unwrap();
    let before = read(&db.node, Collection::Tools, TOOLS_ID).await;
    let error = call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"self_config":{"enable_self_config":false}}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("no-lockout"), "{error}");
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"tools_id":"replacement"}}),
    )
    .await
    .expect_err("identity is protected");
    assert_eq!(read(&db.node, Collection::Tools, TOOLS_ID).await, before);
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["tools"], false, false),
    );
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"self_config":{"enable_self_config":false}}}),
    )
    .await
    .unwrap();
    assert_eq!(
        read(&db.node, Collection::Tools, TOOLS_ID).await["self_config"]["enable_self_config"],
        false
    );
}

#[tokio::test]
async fn get_my_config_redacts_secrets_and_preview_does_not_write() {
    let db = test_db("self-config-read").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["behavior", "tools", "profile", "backend"], false, true),
    );
    let output = call_tool(&tools, "get_my_config", json!({})).await.unwrap();
    let config: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(config["behavior"]["behavior_id"], BEHAVIOR_ID);
    assert_eq!(config["context"]["context_id"], CONTEXT_ID);
    assert_eq!(config["inference_profile"]["profile_id"], PROFILE_ID);
    assert_eq!(config["documents"]["Tools"]["tools_id"], TOOLS_ID);
    assert_eq!(
        config["documents"]["InferenceBackend"]["backend_id"],
        BACKEND_ID
    );
    assert!(
        !output.contains(SECRET),
        "backend credentials must never leave the read owner"
    );
    let before = read(&db.node, Collection::AgentContext, CONTEXT_ID).await;
    let preview=call_tool(&tools,"get_my_config",json!({"preview":{"category":"behavior","kind":"context","patch":{"system_prompt":"previewed prompt"}}})).await.unwrap();
    assert!(
        preview.contains("previewed prompt") && preview.contains("dry-run"),
        "{preview}"
    );
    assert_eq!(
        read(&db.node, Collection::AgentContext, CONTEXT_ID).await,
        before
    );
}

#[tokio::test]
async fn typed_patch_values_reject_injection_and_protected_auth_without_writes() {
    let db = test_db("self-config-injection").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["backend"], false, false),
    );
    let before = read(&db.node, Collection::InferenceBackend, BACKEND_ID).await;
    for patch in [
        json!({"endpoint":{r#"x: 1 }, auth: {kind: "api_key", key: "injected"}, endpoint: "http://injected/v1""#:1}}),
        json!({"auth":{"kind":"api_key","key":"replacement"}}),
    ] {
        call_tool(&tools, "configure_backend", json!({"patch":patch}))
            .await
            .expect_err("typed fields and protected auth must reject the whole patch");
        assert_eq!(
            read(&db.node, Collection::InferenceBackend, BACKEND_ID).await,
            before
        );
    }
    assert_eq!(before["auth"]["key"], SECRET);
    // Selecting a reference replaces the auth variant; there is no second key
    // field to remain accidentally active beside it. Raw key replacement above
    // stays operator-only.
    call_tool(
        &tools,
        "configure_backend",
        json!({"patch":{"auth":{"kind":"environment","variable":"GENTS_TEST_API_KEY"}}}),
    )
    .await
    .unwrap();
    assert_eq!(
        read(&db.node, Collection::InferenceBackend, BACKEND_ID).await["auth"],
        json!({"kind":"environment","variable":"GENTS_TEST_API_KEY"})
    );
}

#[tokio::test]
async fn configure_event_source_rejects_filter_and_collection_injection() {
    let db = test_db("self-config-source-injection").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["automation"], false, false),
    );
    for filter in [
        json!(
            r#"{} ] }, limit: 1) { _docID } AgentBehavior(filter: { _and: [ {} ] }, limit: 1) { context_id } X(filter: { _and: [ {}"#
        ),
        json!("{ a: 1 } # "),
        json!("{ a: 1 }) { x } ("),
        json!({r#"x: 1 }) { _docID } create_AgentBehavior(input: { behavior_id: "evil-injected" }) { _docID } #"#:1}),
    ] {
        call_tool(&tools,"configure_automation",json!({"kind":"event_source","id":"source","patch":{"source_collection":"CustomerSignup","filter":filter}})).await.expect_err("invalid filter rejected before publication");
    }
    for source in [
        "CustomerSignup) { _docID }",
        "CustomerSignup #",
        "CustomerSignup {",
        "A B",
        "",
    ] {
        call_tool(
            &tools,
            "configure_automation",
            json!({"kind":"event_source","id":"source","patch":{"source_collection":source}}),
        )
        .await
        .expect_err("collection identifiers cannot contain GraphQL syntax");
    }
    let rows = ConfigAccess::Local(db.node.clone())
        .execute("{ EventSource { _docID } AgentBehavior { behavior_id } }")
        .await
        .unwrap();
    assert!(rows["data"]["EventSource"].as_array().unwrap().is_empty());
    assert_eq!(rows["data"]["AgentBehavior"].as_array().unwrap().len(), 1);
    call_tool(&tools,"configure_automation",json!({"kind":"event_source","id":"source","patch":{"source_collection":"CustomerSignup","filter":r#"{ kind: { _eq: "signup" } }"#}})).await.unwrap();
    assert_eq!(
        read(&db.node, Collection::EventSource, "source").await["source_collection"],
        "CustomerSignup"
    );
}

#[tokio::test]
async fn configure_automation_creates_one_chain_and_preserves_runtime_ownership() {
    let db = test_db("self-config-automation").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["automation"], false, false),
    );
    for (kind, id, patch) in [
        (
            "task",
            "nightly",
            json!({"display_name":"Nightly review","prompt_template":"Review yesterday's sessions"}),
        ),
        (
            "schedule",
            "cadence",
            json!({"cadence":{"kind":"cron","expression":"0 3 * * *","timezone":"UTC"}}),
        ),
        (
            "trigger",
            "nightly-trigger",
            json!({"task_id":"nightly","source":{"kind":"schedule","schedule_id":"cadence"}}),
        ),
    ] {
        call_tool(
            &tools,
            "configure_automation",
            json!({"kind":kind,"id":id,"patch":patch}),
        )
        .await
        .unwrap();
    }
    let before = read(&db.node, Collection::Trigger, "nightly-trigger").await;
    call_tool(
        &tools,
        "configure_automation",
        json!({"kind":"trigger","id":"nightly-trigger","patch":{"fire_count":0}}),
    )
    .await
    .expect_err("runtime observations are protected");
    assert_eq!(
        read(&db.node, Collection::Trigger, "nightly-trigger").await,
        before
    );
    call_tool(
        &tools,
        "configure_automation",
        json!({"kind":"task","id":"nightly","patch":{"behavior_id":"someone-else"}}),
    )
    .await
    .expect_err("task behavior is protected");
    assert_eq!(
        read(&db.node, Collection::Task, "nightly").await["behavior_id"],
        BEHAVIOR_ID
    );
}

#[tokio::test]
async fn self_only_boundaries_reject_foreign_references_and_corrupted_bindings() {
    let db = test_db("self-config-boundaries").await;
    seed_config(&db.node).await;
    // Corrupt storage explicitly to exercise the self-config reader's fail-closed
    // behavior independently of the normal writer's reference validation.
    for query in [
        r#"mutation { create_Tools(input:{agent_did:"did:key:zVictim", tools_id:"victim-tools"}) {_docID} }"#,
        r#"mutation { create_Task(input:{agent_did:"did:key:zVictim", task_id:"victim-task", behavior_id:"victim", prompt_template:"Victim"}) {_docID} }"#,
    ] {
        let response = db.node.execute(query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.into(),
        None,
        &tool_config(&["behavior", "tools", "automation"], false, false),
    );
    call_tool(&tools,"configure_automation",json!({"kind":"schedule","id":"cadence","patch":{"cadence":{"kind":"interval","interval_secs":60}}})).await.unwrap();
    call_tool(&tools,"configure_automation",json!({"kind":"trigger","id":"foreign-trigger","patch":{"task_id":"victim-task","source":{"kind":"schedule","schedule_id":"cadence"}}})).await.expect_err("foreign task cannot be bound by logical ID");
    let before = read(&db.node, Collection::AgentContext, CONTEXT_ID).await;
    call_tool(
        &tools,
        "configure_behavior",
        json!({"target":"context","patch":{"tools_id":"victim-tools","system_prompt":"hijacked"}}),
    )
    .await
    .expect_err("foreign tools must reject entire context patch");
    assert_eq!(
        read(&db.node, Collection::AgentContext, CONTEXT_ID).await,
        before
    );
    // A sibling behavior under the same principal is still outside this
    // self-config tool's task scope. Retargeting must validate the stored task
    // as well as the proposed one, so it cannot take over a sibling trigger.
    for query in [
        format!(
            r#"mutation {{ create_AgentBehavior(input:{{agent_did:"{AGENT_DID}", behavior_id:"sibling", context_id:"{CONTEXT_ID}", inference_profile_id:"{PROFILE_ID}"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Task(input:{{agent_did:"{AGENT_DID}", task_id:"sibling-task", behavior_id:"sibling", prompt_template:"Sibling work"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Trigger(input:{{agent_did:"{AGENT_DID}", trigger_id:"sibling-trigger", task_id:"sibling-task", source:{{kind:"schedule",schedule_id:"cadence"}}}}) {{_docID}} }}"#
        ),
    ] {
        let response = db.node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    call_tool(
        &tools,
        "configure_automation",
        json!({"kind":"task", "id":"my-task", "patch":{"prompt_template":"Own work"}}),
    )
    .await
    .unwrap();
    call_tool(
        &tools,
        "configure_automation",
        json!({"kind":"trigger", "id":"sibling-trigger", "patch":{"task_id":"my-task"}}),
    )
    .await
    .expect_err("a sibling trigger cannot be taken over by rebinding its task");
    assert_eq!(
        read(&db.node, Collection::Trigger, "sibling-trigger").await["task_id"],
        "sibling-task"
    );
    let response=db.node.execute(&format!(r#"mutation {{ update_AgentContext(filter:{{agent_did:{{_eq:"{AGENT_DID}"}}, context_id:{{_eq:"{CONTEXT_ID}"}}}}, input:{{tools_id:"victim-tools"}}) {{_docID}} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"host":{"bash":{"mode":"Unrestricted"}}}}),
    )
    .await
    .expect_err("corrupt binding must not grant access to foreign tools");
    let response=db.node.execute(r#"{ Tools(filter:{agent_did:{_eq:"did:key:zVictim"}, tools_id:{_eq:"victim-tools"}}) {host} }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert!(response.data.unwrap()["Tools"][0]["host"].is_null());
}

#[tokio::test]
async fn writes_require_an_acp_addressable_agent_identity() {
    let db = test_db("self-config-identity").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        "not-a-did".into(),
        None,
        &tool_config(&["behavior"], false, false),
    );
    let error = call_tool(
        &tools,
        "configure_behavior",
        json!({"target":"context","patch":{"system_prompt":"should not land"}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("ACP-addressable"), "{error}");
    assert_eq!(
        read(&db.node, Collection::AgentContext, CONTEXT_ID).await["system_prompt"],
        "original prompt"
    );
}
