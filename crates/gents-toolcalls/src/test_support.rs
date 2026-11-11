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

/// Makes the process retained-record events visible to a scoped subscriber:
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

/// A served embedded node for tests at this layer: the runtime's trial home
/// is a root-crate owner, so fixtures here start the node from DefraDB's
/// builder with the same node identity, ACP and HTTP GraphQL endpoint.
pub struct TestHome {
    pub node: std::sync::Arc<defra_node::Node>,
    pub node_identity: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    endpoint: String,
    _dir: Option<tempfile::TempDir>,
}

impl TestHome {
    pub async fn create(name: &str) -> anyhow::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("gents-{name}-"))
            .tempdir()?;
        let mut home = Self::create_at(dir.path(), None).await?;
        home._dir = Some(dir);
        Ok(home)
    }

    /// Creates a home at the not yet existing `path`, retained when dropped.
    pub async fn create_at(
        path: &std::path::Path,
        p2p: Option<defra_node::P2PConfig>,
    ) -> anyhow::Result<Self> {
        use gents_store::identity::NodeIdentity as _;
        std::fs::create_dir_all(path.join("data"))?;
        let key_path = path.join("node.key");
        let identity = tokio::task::spawn_blocking(move || {
            gents_store::identity::KeyIdentity::load_or_create(key_path, None)
        })
        .await??;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
        let http = defra_node::HttpConfig::with_listener(listener.into_std()?)?;
        let mut builder = defra_node::Node::builder()
            .data_path(path.join("data"))
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_http(http);
        if let Some(p2p) = p2p {
            builder = builder.with_p2p(p2p);
        }
        let node = std::sync::Arc::new(builder.build().await?);
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        Ok(Self {
            node,
            node_identity: std::sync::Arc::new(identity),
            endpoint,
            _dir: None,
        })
    }

    pub fn client_access(
        &self,
        identity: &dyn gents_store::identity::NodeIdentity,
    ) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::Graphql(
            gents_store::config_client::GraphqlEndpoint::as_principal(
                self.endpoint.clone(),
                identity.did(),
            )
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
        )
    }
}

/// A served node whose runtime has granted `user` the operator role and whose
/// default Agent is provisioned and installed.
pub async fn signed_test_node(
    name: &str,
) -> (
    TestHome,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let home = TestHome::create(name).await.expect("test home");
    signed_test_node_from_home(home, name).await
}

pub async fn signed_test_node_from_home(
    home: TestHome,
    name: &str,
) -> (
    TestHome,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::{GrantSet, NodeIdentity};

    let fixture_name = name.to_owned();
    let user = tokio::task::spawn_blocking(move || gents_store::test_support::test_identity(&fixture_name))
        .await
        .unwrap();
    let runtime = home.node_identity.as_ref();
    assert_ne!(runtime.did(), user.did());
    let admin = ConfigAccess::local_as(home.node.clone(), identity::Did::new(runtime.did()).unwrap());
    let user_did = identity::Did::new(user.did()).unwrap();
    GrantSet::OperatorUser.grant(&admin, &user_did).await.unwrap();
    let access = home.client_access(&user);
    gents_documents::test_support::bootstrap_signed_test_node(&access, runtime.did()).await;
    let binding =
        gents_store::identity::provision_agent_identity_authorized(&home.node, runtime, &user_did)
            .await
            .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        runtime.did(),
        "Default fixture agent",
        &binding,
        &access,
    )
    .await;
    gents_documents::test_support::set_test_default_agent_as(&access, runtime.did(), &binding.agent_did)
        .await;
    (home, user, access, binding)
}

// ---- installed plugin fixtures ----

use gents_pack::plugin::store::{self, InstalledPlugin};
use gents_pack::plugin::test_fixtures::{build_plugin_pack, ECHO_WAT};

pub fn installed_echo() -> (tempfile::TempDir, InstalledPlugin) {
    installed_plugin(ECHO_WAT, None)
}

pub fn installed_plugin(
    wat: &str,
    access: Option<gents_pack::pack::BindAccess>,
) -> (tempfile::TempDir, InstalledPlugin) {
    let (mut declaration, bytes) = build_plugin_pack("echo_pack", wat, None);
    declaration.bind_dir = access.map(|access| gents_pack::pack::PluginDirBinding {
        input_field: "path".into(),
        original_field: None,
        description: "a directory".into(),
        access,
        write_fields: Vec::new(),
    });
    let home = tempfile::tempdir().unwrap();
    let hex = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes));
    store::store_bytes(home.path(), &hex, &bytes).unwrap();
    let record = InstalledPlugin {
        namespace: "team".into(),
        name: declaration.name.clone(),
        version: "0.1.0".into(),
        digest: format!("sha256:{hex}"),
        language: "rust".into(),
        declaration,
        granted: None,
        instructions: None,
        owner_pack_coordinate: None,
        owner_pack_digest: None,
        model_binding: None,
        data_unique_keys: Default::default(),
        data_collection_fields: Default::default(),
        data_collection_schemas: Default::default(),
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
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

pub fn asking_plugin_wat(
    service: &str,
    results: &str,
    canned: &serde_json::Value,
    forever: bool,
) -> String {
    let escape = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let canned = canned.to_string();
    let calls = format!("\"{service}\":true");
    let stop = if forever {
        "(i32.const 1)".to_owned()
    } else {
        format!(
            "(i32.eqz (call $contains (i32.const 50000) (i32.const {}) (local.get $n)))",
            results.len()
        )
    };
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 8192) "{canned_bytes}")
  (data (i32.const 50000) "{results_bytes}")
  (data (i32.const 50100) "{calls_bytes}")
  (func $contains (param $needle i32) (param $nlen i32) (param $hlen i32) (result i32)
    (local $i i32) (local $j i32)
    (block $notfound
      (loop $outer
        (br_if $notfound (i32.gt_u (i32.add (local.get $i) (local.get $nlen)) (local.get $hlen)))
        (local.set $j (i32.const 0))
        (block $mismatch
          (loop $inner
            (if (i32.eq (local.get $j) (local.get $nlen)) (then (return (i32.const 1))))
            (br_if $mismatch (i32.ne
              (i32.load8_u (i32.add (local.get $i) (local.get $j)))
              (i32.load8_u (i32.add (local.get $needle) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $inner)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (i32.const 0))
  (func (export "_start")
    (local $n i32)
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (i32.const 8000))
    (drop (call $fd_read (i32.const 0) (i32.const 60000) (i32.const 1) (i32.const 60008)))
    (local.set $n (i32.load (i32.const 60008)))
    (if (call $contains (i32.const 50100) (i32.const {calls_len}) (local.get $n))
      (then
        (if {stop}
          (then
            (i32.store (i32.const 60000) (i32.const 8192))
            (i32.store (i32.const 60004) (i32.const {canned_len}))
            (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))
            (return)))))
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (local.get $n))
    (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))))"#,
        canned_bytes = escape(&canned),
        results_bytes = escape(results),
        calls_bytes = escape(&calls),
        calls_len = calls.len(),
        canned_len = canned.len(),
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

