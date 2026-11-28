pub mod background_completion;
pub mod doubles;
#[cfg(unix)]
pub mod held_process;
pub mod plugin_job;
pub mod self_config;

#[cfg(any(test, feature = "test-fixtures"))]
use gents_pack::test_support::fixture_pack_copy;

#[cfg(any(test, feature = "test-fixtures"))]
pub fn home_with_fixture_pack(
    name: &str,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::plugin::executor::PluginExecutor>,
) {
    let (_guard, dir) = fixture_pack_copy(name, &serde_json::json!({}));
    home_with_pack_dir(&dir)
}

pub fn home_with_pack_dir(
    dir: &std::path::Path,
) -> (
    tempfile::TempDir,
    std::sync::Arc<crate::plugin::executor::PluginExecutor>,
) {
    let (bytes, _) = gents_pack::pack::archive::pack_dir(dir)
        .unwrap_or_else(|error| panic!("packing {}: {error:#}", dir.display()));
    let home = tempfile::tempdir().expect("home");
    gents_pack::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap_or_else(|error| panic!("storing {}: {error:#}", dir.display()));
    let plugins = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_path_buf(),
    )));
    (home, plugins)
}

pub fn default_plugin_host() -> std::sync::Arc<dyn gents_tools::plugin_host::PluginHost> {
    std::sync::Arc::new(crate::plugin::executor::PluginExecutor::default())
}

/// An Agent identity watch that publishes a fixed set of signers and never
/// changes.
pub struct FixedAgentIdentities(Vec<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>);

impl FixedAgentIdentities {
    pub fn boxed(
        identities: Vec<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>,
    ) -> Box<dyn gents_store::agent_identity_watch::AgentIdentityWatch> {
        Box::new(Self(identities))
    }
}

impl gents_store::agent_identity_watch::AgentIdentityWatch for FixedAgentIdentities {
    fn watcher_identities(
        &self,
    ) -> anyhow::Result<Vec<std::sync::Arc<dyn gents_store::identity::NodeIdentity>>> {
        Ok(self.0.clone())
    }

    fn watched_and_active_agents(
        &self,
    ) -> anyhow::Result<gents_store::agent_identity_watch::WatchedAndActiveAgents> {
        Ok((self.0.clone(), Default::default()))
    }

    fn changed(
        &mut self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<(), gents_store::agent_identity_watch::AgentIdentityWatchClosed>,
    > {
        Box::pin(std::future::pending())
    }
}

/// Makes the process retained-record and plugin publication wait events
/// visible to a scoped subscriber:
/// tracing caches callsite interest globally, so a scoped subscriber sees an
/// event only when a global default has enabled its target first.
#[cfg(test)]
pub(crate) fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(CapturedTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

#[cfg(test)]
struct CapturedTargetSubscriber;

#[cfg(test)]
impl tracing::Subscriber for CapturedTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target().starts_with("gents_toolcalls::call::hook")
            || metadata.target() == gents_tools::surface::PLUGIN_PUBLICATION_WAIT_EVENT_TARGET
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

pub fn host_call_plugin_wat(request: &serde_json::Value, count: u32) -> String {
    let bytes = request.to_string();
    let escaped = bytes
        .bytes()
        .map(|b| format!("\\{b:02x}"))
        .collect::<String>();
    format!(
        r#"(module
      (import "gents" "host_call" (func $call (param i32 i32) (result i32)))
      (import "gents" "read_result" (func $read (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 2)
      (data (i32.const 32) "{escaped}")
      (func (export "_start") (local $n i32) (local $i i32)
        (loop $again
          (local.set $n (call $call (i32.const 32) (i32.const {length})))
          (if (i32.lt_s (local.get $n) (i32.const 0)) (then unreachable))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br_if $again (i32.lt_u (local.get $i) (i32.const {count}))))
        (drop (call $read (i32.const 65536) (local.get $n)))
        (i32.store (i32.const 0) (i32.const 65536))
        (i32.store (i32.const 4) (local.get $n))
        (drop (call $write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))"#,
        length = bytes.len()
    )
}

/// Inserts a document through `access` and returns its doc id. Rows owned
/// by an Agent must be written through that Agent's access.
pub async fn insert_as(
    access: &gents_store::config_client::ConfigAccess,
    collection: &str,
    value: serde_json::Value,
) -> String {
    access
        .transact_write("test.plugin_job_children", |txn| {
            let value = &value;
            Box::pin(async move {
                let query = format!(
                    "mutation($input: {collection}MutationInputArg!) {{create_{collection}(input:$input){{_docID}}}}"
                );
                let response = txn
                    .execute_with_variables(&query, &serde_json::json!({ "input": value }))
                    .await?;
                Ok::<_, anyhow::Error>(gents_protocol::graphql::extract_mutation_doc_id(&response, collection)?)
            })
        })
        .await
        .unwrap()
}

pub fn live_settings(
    policy: gents_protocol::job::JobPolicy,
) -> gents_protocol::job::PluginJobSettings {
    gents_protocol::job::PluginJobSettings {
        agent_targets: vec!["a".into(), "b".into()],
        max_live: 3,
        max_spawns: 10,
        max_messages_per_slot: 2,
        result_bytes: 2048,
        max_wall_secs: 600,
        policy,
    }
}

pub async fn write_config_with_access<T>(
    access: &gents_store::config_client::ConfigAccess,
    collection: gents_store::collection::Collection,
    document: &T,
) where
    T: serde::Serialize,
{
    let value = serde_json::to_value(document).unwrap();
    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    access
        .transact_write("test.write_config", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
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

pub async fn enable_agents_tools_with_access(
    access: &gents_store::config_client::ConfigAccess,
    did: &str,
    agent_did: &str,
) {
    write_config_with_access(
        access,
        gents_store::collection::Collection::AgentTarget,
        &gents_documents::AgentTargetDocument {
            target_id: "general:general".to_owned(),
            node_did: did.to_owned(),
            target_node_did: did.to_owned(),
            agent_did: agent_did.to_owned(),
            name: "general".to_owned(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await;
    write_config_with_access(
        access,
        gents_store::collection::Collection::Tools,
        &serde_json::json!({
            "node_did": did,
            "tools_id": format!("{agent_did}:tools"),
            "agents": { "enabled": true, "target_ids": ["general:general"] }
        }),
    )
    .await;
    write_config_with_access(
        access,
        gents_store::collection::Collection::Task,
        &serde_json::json!({
            "node_did": did,
            "task_id": "goal",
            "agent_did": agent_did,
            "prompt_template": "pursue the goal",
            "goal_objective_template": "finish the work"
        }),
    )
    .await;
}

/// Grants `subject` the direct sets `GrantSet::for_recipient` gives
/// `recipient`, as the runtime owner does at init. The fixture Agent is its
/// Node's default Agent, so it holds the modeled default-Agent sets.
pub async fn grant_modeled_direct_sets(
    node: &std::sync::Arc<defra_node::Node>,
    runtime_did: &str,
    recipient: gents_store::identity::Recipient,
    subject: &str,
) {
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime_did).unwrap(),
    );
    let subject = identity::Did::new(subject).unwrap();
    gents_store::identity::GrantSet::for_recipient(recipient)
        .grant_direct(&admin, &subject)
        .await
        .unwrap();
}

pub fn caller_agent_did(
    message: &crate::call::lifecycle::admission_fixture::PublishedSessionMessage,
) -> String {
    message.admission.tool.actor().to_string()
}

pub mod stream_processor;

pub mod queue;
pub mod workspace_rejection;

pub mod background_hook;

pub mod script_composition;
