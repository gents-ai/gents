use std::sync::Arc;

use gents_documents::apply_desired_state_plan;
use gents_documents::{
    read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_tools::builtin::self_config::build_runtime_self_config_tools;
use gents_tools::surface::SelfConfigToolConfig;
use serde_json::{json, Value};

use crate::support::agent_requests::RequestActors;
use crate::support::trial_store;
use gents_store::test_support::TestStore;

fn test_plugins() -> Arc<gents_toolcalls::plugin::executor::PluginExecutor> {
    Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::default())
}

const CONTEXT_ID: &str = "self-config-context";
const TOOLS_ID: &str = "self-config-tools";
const PROFILE_ID: &str = "self-config-profile";
const BACKEND_ID: &str = "self-config-backend";
const SECRET: &str = "sk-secret-should-never-leak";

/// The enrolled User configures the Node and its Agent; the self-config tool
/// runs as that Agent's managed signer.
struct SelfConfig {
    db: TestStore,
    actors: RequestActors,
}

impl SelfConfig {
    async fn new(name: &str) -> Self {
        let db = trial_store(name).await;
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
    ) -> Vec<Box<dyn gents_loop::tool::ToolDyn>> {
        build_runtime_self_config_tools(
            std::sync::Arc::default(),
            self.db.node.clone(),
            self.actors.agent().clone(),
            self.db.node_identity.clone(),
            config,
            test_plugins(),
            std::sync::Arc::new(gents_automation::triggers::task_run::TaskRuns),
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
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }

    async fn seed(&self) {
        let node_did = self.node_did().to_owned();
        let node = self.read(Collection::Node, &node_did).await;
        let mut agent = self.read(Collection::Agent, self.agent_did()).await;
        agent["context_id"] = CONTEXT_ID.into();
        agent["inference_profile_id"] = PROFILE_ID.into();
        self.apply(vec![
            (Collection::Node, node),
            (
                Collection::DefaultAgentSelection,
                json!({"node_did":node_did,"default_agent_did":self.agent_did()}),
            ),
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
                    gents_documents::grant_selected_self_configuration(txn, node_did, agent_did)
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
        gents_store::identity::GrantSet::PolicyDelegate
            .grant(&admin, &identity::Did::new(agent_did).unwrap())
            .await
            .expect("init grants the default Agent its direct set");
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
        memory_enabled: false,
        categories: categories.iter().map(|s| s.to_string()).collect(),
        no_lockout,
        preview,
        enable_pack_install: false,
        enable_pack_authority: false,
        process_ceiling: Default::default(),
        selected_plugins: Vec::new(),
        ceiling_plugins: None,
        registry_url: None,
        plugin_builder: None,
        ux_preview_assets: None,
    }
}

/// `task run` fires through the runtime's real `TaskRuns` service, which
/// lives above the tool crate.
#[tokio::test]
async fn task_run_fires_a_task_now_and_reuses_target_id() {
    let f = SelfConfig::new("self-config-task-run").await;
    let tools = f
        .tools(&f.config(&["tools", "automation"], false, false))
        .await;
    let config = |argv: Value| {
        let tools = &tools;
        async move {
            tools
                .iter()
                .find(|tool| tool.name() == "config")
                .unwrap()
                .call(
                    argv.to_string(),
                    &gents_loop::tool::ToolCallEnvelope::default(),
                )
                .await
        }
    };
    config(json!({"argv":["task","create","review","--set",r#"prompt_template="Review {{ args.what }}""#]}))
        .await
        .unwrap();

    let text = config(
        json!({"argv":["task","run"],"target_id":"review","options":{"args":{"what":"the bill"}}}),
    )
    .await
    .unwrap();
    let fired: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(fired["status"], "pending", "{fired}");
    assert_eq!(fired["duplicate"], false, "{fired}");
    let request_id = fired["request_id"].as_str().unwrap();
    let query = format!(
        r#"query {{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ content }} }}"#,
        escape_graphql_string(request_id)
    );
    let response = f.actors.agent_access.execute(&query).await.unwrap();
    assert_eq!(
        response["data"]["AgentRequest"][0]["content"], "Review the bill",
        "{response}"
    );

    let missing = config(json!({"argv":["task","run","absent"]}))
        .await
        .unwrap_err()
        .to_string();
    assert!(missing.contains("task absent was not found"), "{missing}");
}
