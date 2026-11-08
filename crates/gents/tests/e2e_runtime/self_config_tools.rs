use std::sync::Arc;

use gents::store::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::store::graphql::escape_graphql_string;
use gents::tools::builtin::self_config::build_runtime_self_config_tools;
use gents::tools::surface::{
    BashMode, FileToolMode, SelfConfigProcessCeiling, SelfConfigToolConfig,
};
use gents::Collection;
use serde_json::{json, Value};

use crate::support::agent_requests::RequestActors;
use crate::support::{test_db, TestDb};

fn test_plugins() -> Arc<gents::plugin::executor::PluginExecutor> {
    Arc::new(gents::plugin::executor::PluginExecutor::default())
}

const CONTEXT_ID: &str = "self-config-context";
const TOOLS_ID: &str = "self-config-tools";
const PROFILE_ID: &str = "self-config-profile";
const BACKEND_ID: &str = "self-config-backend";
const SECRET: &str = "sk-secret-should-never-leak";

/// The enrolled User configures the Node and its Agent; the self-config tool
/// runs as that Agent's managed signer.
struct SelfConfig {
    db: TestDb,
    actors: RequestActors,
}

impl SelfConfig {
    async fn new(name: &str) -> Self {
        let db = test_db(name).await;
        let actors = RequestActors::new(&db).await;
        let fixture = Self { db, actors };
        fixture.seed().await;
        fixture
    }

    fn node_did(&self) -> &str {
        self.actors.node_did()
    }

    fn agent_did(&self) -> &str {
        self.actors.agent_did()
    }

    fn user(&self) -> &ConfigAccess {
        &self.actors.fixture.access
    }

    /// The config tool as the runtime surface registers it for the Agent.
    async fn tools(
        &self,
        config: &SelfConfigToolConfig,
    ) -> Vec<Box<dyn gents::inference::llm::tool::ToolDyn>> {
        build_runtime_self_config_tools(
            std::sync::Arc::default(),
            self.db.node.clone(),
            self.actors.agent().clone(),
            self.db.node_identity.clone(),
            config,
            test_plugins(),
        )
        .await
        .expect("runtime registers the Agent's config tool")
    }

    fn config(&self, categories: &[&str], no_lockout: bool, preview: bool) -> SelfConfigToolConfig {
        tool_config(self.agent_did(), categories, no_lockout, preview)
    }

    /// Applies desired-state documents as the configuring User.
    async fn apply(&self, documents: Vec<(Collection, Value)>) {
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.user()
            .transact_write("test.self_config.seed", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
    }

    async fn seed(&self) {
        let node_did = self.node_did().to_owned();
        let mut node = self.read(Collection::Node, &node_did).await;
        node["default_agent_did"] = self.agent_did().into();
        let mut agent = self.read(Collection::Agent, self.agent_did()).await;
        agent["context_id"] = CONTEXT_ID.into();
        agent["inference_profile_id"] = PROFILE_ID.into();
        self.apply(vec![
            (Collection::Node, node),
            (Collection::InferenceBackend, json!({"node_did":node_did,"backend_id":BACKEND_ID,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:11434/v1","auth":{"kind":"api_key","key":SECRET},"max_concurrent":1})),
            (Collection::InferenceSampling, json!({"node_did":node_did,"sampling_id":"sampling","temperature":0.7})),
            (Collection::InferenceExecution, json!({"node_did":node_did,"execution_id":"execution","max_turns":40})),
            (Collection::InferenceProfile, json!({"node_did":node_did,"profile_id":PROFILE_ID,"backend_id":BACKEND_ID,"model_name":"model-small","sampling_id":"sampling","execution_id":"execution"})),
            (Collection::Tools, json!({"node_did":node_did,"tools_id":TOOLS_ID,"self_config":{"enable_self_config":true}})),
            (Collection::AgentContext, json!({"node_did":node_did,"context_id":CONTEXT_ID,"system_prompt":"original prompt","tools_id":TOOLS_ID})),
            (Collection::Agent, agent),
        ])
        .await;
        // The User grants the Agent authority over its selected documents.
        let agent_did = self.agent_did();
        self.user()
            .transact_write("test.self_config.grant_selected", |txn| {
                let node_did = &node_did;
                Box::pin(async move {
                    gents::store::config_client::grant_selected_self_configuration(
                        txn, node_did, agent_did,
                    )
                    .await
                })
            })
            .await
            .expect("User grants selected configuration to the Agent");
        // The Agent is the Node's default Agent, which init issues the
        // policy-delegate set (Lean `GrantSets.directSets .defaultAgent`).
        let admin = ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.node_did()).unwrap(),
        );
        gents::store::identity::GrantSet::PolicyDelegate
            .grant(&admin, &identity::Did::new(agent_did).unwrap())
            .await
            .expect("init grants the default Agent its direct set");
    }

    /// The configuring User grants the Agent a direct write on one document.
    async fn grant_writer(&self, collection: Collection, id: &str) {
        let node_did = self.node_did();
        let agent = identity::Did::new(self.agent_did()).unwrap();
        self.user()
            .transact_write("test.self_config.grant_writer", |txn| {
                let agent = &agent;
                Box::pin(async move {
                    let (doc, _) = read_desired_state_record_in_txn(txn, collection, node_did, id)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("{collection:?} {id} exists"))?;
                    let name = if collection == Collection::InferenceBackend {
                        gents_protocol::schemas::INFERENCE_BACKEND_METADATA_NAME
                    } else {
                        collection.graphql_type()
                    };
                    txn.add_actor_relationship(name, &doc, "writer", agent)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .expect("User grants the Agent a direct writer");
    }

    /// Reads a document as the configuring User, which owns the configuration.
    async fn read(&self, collection: Collection, id: &str) -> Value {
        self.try_read(collection, self.node_did(), id)
            .await
            .unwrap_or_else(|| panic!("{collection:?} {id} exists"))
    }

    async fn try_read(&self, collection: Collection, node_did: &str, id: &str) -> Option<Value> {
        self.user()
            .transact_readonly("test.self_config.read", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        read_desired_state_record_in_txn(txn, collection, node_did, id)
                            .await?
                            .map(|(_, value)| value),
                    )
                })
            })
            .await
            .unwrap()
    }
}

fn tool_config(
    agent_did: &str,
    categories: &[&str],
    no_lockout: bool,
    preview: bool,
) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        agent_did: agent_did.into(),
        categories: categories.iter().map(|s| s.to_string()).collect(),
        no_lockout,
        preview,
        enable_pack_install: false,
        process_ceiling: Default::default(),
    }
}

async fn call_tool(
    tools: &[Box<dyn gents::inference::llm::tool::ToolDyn>],
    name: &str,
    args: Value,
) -> Result<String, String> {
    let argv = legacy_test_call_as_config_argv(name, &args);
    tools
        .iter()
        .find(|tool| tool.name() == "config")
        .unwrap_or_else(|| panic!("missing config tool for {name}"))
        .call(json!({"argv": argv}).to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

fn legacy_test_call_as_config_argv(name: &str, args: &Value) -> Vec<String> {
    let patch = |mut argv: Vec<String>, value: &Value| {
        for (field, value) in value.as_object().expect("patch object") {
            argv.extend(["--set".into(), format!("{field}={value}")]);
        }
        argv
    };
    match name {
        "get_my_config" if args.get("preview").is_none() => vec!["get".into()],
        "get_my_config" => {
            let preview = &args["preview"];
            let category = preview["category"].as_str().unwrap();
            let kind = preview.get("kind").and_then(Value::as_str);
            let head = match (category, kind) {
                ("agent", Some("context")) => {
                    vec!["agent".into(), "context".into(), "preview".into()]
                }
                ("tools", _) => vec!["tools".into(), "preview".into()],
                ("profile", target) => {
                    let mut head = vec!["profile".into(), "preview".into()];
                    if let Some(target) = target {
                        head.push(target.replace('_', "-"));
                    }
                    head
                }
                ("backend", _) => vec!["backend".into(), "preview".into()],
                _ => panic!("unsupported preview in config e2e: {preview}"),
            };
            patch(head, &preview["patch"])
        }
        "configure_agent" => {
            assert_eq!(args.get("target").and_then(Value::as_str), Some("context"));
            patch(
                vec!["agent".into(), "context".into(), "edit".into()],
                &args["patch"],
            )
        }
        "configure_tools" => patch(vec!["tools".into(), "edit".into()], &args["patch"]),
        "configure_profile" => {
            let mut head = vec!["profile".into(), "edit".into()];
            if let Some(target) = args.get("target").and_then(Value::as_str) {
                head.push(target.replace('_', "-"));
            }
            patch(head, &args["patch"])
        }
        "configure_backend" => patch(vec!["backend".into(), "edit".into()], &args["patch"]),
        "configure_automation" => patch(
            vec![
                "automation".into(),
                "edit".into(),
                args["kind"].as_str().unwrap().replace('_', "-"),
                args["id"].as_str().unwrap().into(),
            ],
            &args["patch"],
        ),
        _ => panic!("unsupported test call {name}: {args}"),
    }
}

#[tokio::test]
async fn configure_context_and_profile_preserve_identity_and_reject_partial_commits() {
    let f = SelfConfig::new("self-config-bindings").await;
    let tools = f
        .tools(&f.config(&["agent", "profile"], false, false))
        .await;
    call_tool(
        &tools,
        "configure_agent",
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
        f.read(Collection::AgentContext, CONTEXT_ID).await["system_prompt"],
        "sharper prompt"
    );
    let before = f.read(Collection::InferenceProfile, PROFILE_ID).await;
    assert_eq!(before["model_name"], "model-large");
    for patch in [
        json!({"node_did":"did:key:zAttacker","model_name":"hijacked"}),
        json!({"backend_id":"missing-backend","model_name":"half applied?"}),
    ] {
        call_tool(&tools, "configure_profile", json!({"patch":patch}))
            .await
            .expect_err("identity/reference validation must reject the entire patch");
        assert_eq!(
            f.read(Collection::InferenceProfile, PROFILE_ID).await,
            before
        );
    }
    assert_eq!(
        f.read(Collection::Agent, f.agent_did()).await["node_did"],
        f.node_did()
    );
}

#[tokio::test]
async fn configure_tools_respects_gate_and_no_lockout() {
    let f = SelfConfig::new("self-config-tools").await;
    let tools = f.tools(&f.config(&["tools"], true, false)).await;
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"built_ins":{"enable_context_budget":true}}}),
    )
    .await
    .unwrap();
    let before = f.read(Collection::Tools, TOOLS_ID).await;
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
    assert_eq!(f.read(Collection::Tools, TOOLS_ID).await, before);
    let tools = f.tools(&f.config(&["tools"], false, false)).await;
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"self_config":{"enable_self_config":false}}}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.read(Collection::Tools, TOOLS_ID).await["self_config"]["enable_self_config"],
        false
    );
}

#[tokio::test]
async fn get_my_config_redacts_secrets_and_preview_does_not_write() {
    let f = SelfConfig::new("self-config-read").await;
    let mut config = f.config(&["agent", "tools", "profile", "backend"], false, true);
    config.process_ceiling = SelfConfigProcessCeiling {
        file_mode: FileToolMode::ReadWrite,
        bash_mode: BashMode::Unrestricted,
        root: None,
    };
    let tools = f.tools(&config).await;
    let output = call_tool(&tools, "get_my_config", json!({})).await.unwrap();
    let config: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(config["agent"]["agent_did"], f.agent_did());
    assert_eq!(config["context"]["context_id"], CONTEXT_ID);
    assert_eq!(config["inference_profile"]["profile_id"], PROFILE_ID);
    assert_eq!(config["documents"]["Tools"]["tools_id"], TOOLS_ID);
    assert_eq!(
        config["documents"]["InferenceBackendMetadata"]["backend_id"], BACKEND_ID,
        "{output}"
    );
    assert_eq!(
        config["runtime_effective"]["process_ceiling"]["file_mode"],
        "ReadWrite"
    );
    assert_eq!(
        config["runtime_effective"]["process_ceiling"]["bash_mode"],
        "Unrestricted"
    );
    assert_eq!(
        config["runtime_effective"]["agent_narrowing"]["requested_file_mode"],
        "Off"
    );
    assert_eq!(config["runtime_effective"]["effective"]["file_mode"], "Off");
    assert!(
        !output.contains(SECRET),
        "backend credentials must never leave the read owner"
    );
    let before = f.read(Collection::AgentContext, CONTEXT_ID).await;
    let preview=call_tool(&tools,"get_my_config",json!({"preview":{"category":"agent","kind":"context","patch":{"system_prompt":"previewed prompt"}}})).await.unwrap();
    assert!(
        preview.contains("previewed prompt") && preview.contains("nothing was written"),
        "{preview}"
    );
    assert_eq!(f.read(Collection::AgentContext, CONTEXT_ID).await, before);
}

#[tokio::test]
async fn typed_patch_values_reject_injection_and_protected_auth_without_writes() {
    let f = SelfConfig::new("self-config-injection").await;
    let tools = f.tools(&f.config(&["backend"], false, false)).await;
    let before = f.read(Collection::InferenceBackend, BACKEND_ID).await;
    for patch in [
        json!({"endpoint":{r#"x: 1 }, auth: {kind: "api_key", key: "injected"}, endpoint: "http://injected/v1""#:1}}),
        json!({"auth":{"kind":"api_key","key":"replacement"}}),
    ] {
        call_tool(&tools, "configure_backend", json!({"patch":patch}))
            .await
            .expect_err("typed fields and protected auth must reject the whole patch");
        assert_eq!(
            f.read(Collection::InferenceBackend, BACKEND_ID).await,
            before
        );
    }
    // The seeded key lives only in the backend's secret credential; the
    // metadata names that credential.
    assert_eq!(before["auth"]["kind"], "credential", "{before}");
    let credential = escape_graphql_string(before["auth"]["id"].as_str().unwrap());
    let stored = f
        .user()
        .execute(&format!(
            r#"{{ BackendCredential(filter: {{ credential_id: {{ _eq: "{credential}" }} }}) {{ secret }} }}"#
        ))
        .await
        .unwrap();
    assert!(
        stored["data"]["BackendCredential"][0]["secret"]
            .to_string()
            .contains(SECRET),
        "{stored}"
    );
    // The selected-configuration grant lets the Agent select the backend, not
    // write it (Lean `selected_backend_manager_is_scoped_and_revocable`); the
    // User grants a write explicitly.
    let env_auth = json!({"patch":{"auth":{"kind":"environment","variable":"GENTS_TEST_API_KEY"}}});
    call_tool(&tools, "configure_backend", env_auth.clone())
        .await
        .expect_err("selection authority does not write backend metadata");
    assert_eq!(
        f.read(Collection::InferenceBackend, BACKEND_ID).await,
        before
    );
    f.grant_writer(Collection::InferenceBackend, BACKEND_ID)
        .await;
    call_tool(&tools, "configure_backend", env_auth)
        .await
        .unwrap();
    assert_eq!(
        f.read(Collection::InferenceBackend, BACKEND_ID).await["auth"],
        json!({"kind":"environment","variable":"GENTS_TEST_API_KEY"})
    );
}

#[tokio::test]
async fn configure_event_source_rejects_filter_and_collection_injection() {
    let f = SelfConfig::new("self-config-source-injection").await;
    let tools = f.tools(&f.config(&["automation"], false, false)).await;
    for filter in [
        json!(
            r#"{} ] }, limit: 1) { _docID } Agent(filter: { _and: [ {} ] }, limit: 1) { context_id } X(filter: { _and: [ {}"#
        ),
        json!("{ a: 1 } # "),
        json!("{ a: 1 }) { x } ("),
        json!({r#"x: 1 }) { _docID } create_Agent(input: { agent_did: "evil-injected" }) { _docID } #"#:1}),
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
    let rows = f
        .user()
        .execute("{ EventSource { _docID } Agent { agent_did } }")
        .await
        .unwrap();
    assert!(rows["data"]["EventSource"].as_array().unwrap().is_empty());
    assert_eq!(rows["data"]["Agent"].as_array().unwrap().len(), 1);
    // Collection schemas are installed by the household owner, as `gents init`
    // makes the enrolling User.
    crate::support::grant_household_owner(&f.db, &f.actors.fixture.user).await;
    f.user()
        .add_collection("type CustomerSignup { kind: String }")
        .await
        .unwrap();
    call_tool(&tools,"configure_automation",json!({"kind":"event_source","id":"source","patch":{"source_collection":"CustomerSignup","filter":r#"{ kind: { _eq: "signup" } }"#}})).await.unwrap();
    assert_eq!(
        f.read(Collection::EventSource, "source").await["source_collection"],
        "CustomerSignup"
    );
}

#[tokio::test]
async fn configure_automation_creates_one_chain_and_preserves_runtime_ownership() {
    let f = SelfConfig::new("self-config-automation").await;
    let tools = f.tools(&f.config(&["automation"], false, false)).await;
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
    let before = f.read(Collection::Trigger, "nightly-trigger").await;
    call_tool(
        &tools,
        "configure_automation",
        json!({"kind":"trigger","id":"nightly-trigger","patch":{"fire_count":0}}),
    )
    .await
    .expect_err("runtime observations are protected");
    assert_eq!(f.read(Collection::Trigger, "nightly-trigger").await, before);
    call_tool(
        &tools,
        "configure_automation",
        json!({"kind":"task","id":"nightly","patch":{"agent_did":"someone-else"}}),
    )
    .await
    .expect_err("task agent is protected");
    assert_eq!(
        f.read(Collection::Task, "nightly").await["agent_did"],
        f.agent_did()
    );
}

#[tokio::test]
async fn self_only_boundaries_reject_foreign_references_and_corrupted_bindings() {
    let f = SelfConfig::new("self-config-boundaries").await;
    // A sibling Agent on the same Node, configured by the same User, with its
    // own Tools, Task and Trigger that the fixture Agent holds no grant on.
    let (sibling, _) = crate::support::install_managed_agent(
        &f.db,
        f.user(),
        &f.actors.fixture.user,
        "Sibling agent",
    )
    .await;
    let sibling = gents::NodeIdentity::did(sibling.as_ref()).to_owned();
    let node_did = f.node_did().to_owned();
    f.apply(vec![
        (
            Collection::Tools,
            json!({"node_did":node_did,"tools_id":"victim-tools"}),
        ),
        (
            Collection::Task,
            json!({"node_did":node_did,"task_id":"victim-task","agent_did":sibling,"prompt_template":"Victim"}),
        ),
    ])
    .await;
    let tools = f
        .tools(&f.config(&["agent", "tools", "automation"], false, false))
        .await;
    call_tool(&tools,"configure_automation",json!({"kind":"schedule","id":"cadence","patch":{"cadence":{"kind":"interval","interval_secs":60}}})).await.unwrap();
    call_tool(&tools,"configure_automation",json!({"kind":"trigger","id":"foreign-trigger","patch":{"task_id":"victim-task","source":{"kind":"schedule","schedule_id":"cadence"}}})).await.expect_err("foreign task cannot be bound by logical ID");
    let before = f.read(Collection::AgentContext, CONTEXT_ID).await;
    call_tool(
        &tools,
        "configure_agent",
        json!({"target":"context","patch":{"tools_id":"victim-tools","system_prompt":"hijacked"}}),
    )
    .await
    .expect_err("foreign tools must reject entire context patch");
    assert_eq!(f.read(Collection::AgentContext, CONTEXT_ID).await, before);
    f.apply(vec![
        (
            Collection::Task,
            json!({"node_did":node_did,"task_id":"sibling-task","agent_did":sibling,"prompt_template":"Sibling work"}),
        ),
        (
            Collection::Trigger,
            json!({"node_did":node_did,"trigger_id":"sibling-trigger","task_id":"sibling-task","source":{"kind":"schedule","schedule_id":"cadence"}}),
        ),
    ])
    .await;
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
        f.read(Collection::Trigger, "sibling-trigger").await["task_id"],
        "sibling-task"
    );
    // The configuring User binds the Agent's context to the sibling's Tools;
    // that binding must not grant the Agent access to the foreign Tools.
    let mut corrupted = f.read(Collection::AgentContext, CONTEXT_ID).await;
    corrupted["tools_id"] = "victim-tools".into();
    f.apply(vec![(Collection::AgentContext, corrupted)]).await;
    call_tool(
        &tools,
        "configure_tools",
        json!({"patch":{"host":{"bash":{"mode":"Unrestricted"}}}}),
    )
    .await
    .expect_err("corrupt binding must not grant access to foreign tools");
    assert!(f.read(Collection::Tools, "victim-tools").await["host"].is_null());
}

#[tokio::test]
async fn writes_require_an_acp_addressable_agent_identity() {
    let f = SelfConfig::new("self-config-identity").await;
    let tools = f
        .tools(&tool_config(
            "self-config-behavior",
            &["agent"],
            false,
            false,
        ))
        .await;
    let error = call_tool(
        &tools,
        "configure_agent",
        json!({"target":"context","patch":{"system_prompt":"should not land"}}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("ACP-addressable"), "{error}");
    assert_eq!(
        f.read(Collection::AgentContext, CONTEXT_ID).await["system_prompt"],
        "original prompt"
    );
}
