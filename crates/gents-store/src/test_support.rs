pub mod acp_policies;
pub mod bounded_process;
#[cfg(unix)]
mod executable_script;
pub mod relationships;
pub mod snapshot_reader;
pub mod wire_database;

#[cfg(unix)]
pub use executable_script::{copy_executable, hold_sibling, write_executable_script, HeldSibling};

/// Hang detector for every wait without a deadline of its own: the longest a
/// wait may observe no progress (no change to the documents it watches, no
/// output from the process it waits on) before it fails with the state it
/// last saw. It is the one suite-wide ceiling, far above any host's scheduling
/// delay, never a per-test latency budget (TLA+ `WallClockBound`): the runtime
/// has no quiescence notification to fail on instead, and a fixture that holds
/// a provider response or a process is quiet by design.
pub const STALL_CEILING: std::time::Duration = std::time::Duration::from_secs(600);

/// A fixture deadline the scenario does not exercise: beyond any run the hang
/// ceiling lets finish, so the product's deadline cut is never enabled
/// (`WallClockBound` `DeadlineMode = "unreached"`).
pub const UNREACHED_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(4 * STALL_CEILING.as_secs());

/// [`UNREACHED_DEADLINE`] from now, as the wall-clock instant a fixture binds.
pub fn unreached_deadline_at() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::TimeDelta::from_std(UNREACHED_DEADLINE).expect("bounded")
}

/// Resolves with the first `Ok` that `read` returns, re-running it after each
/// change to `collection` that `reader` observes, and fails with `what` and
/// the last `Err` once `collection` has not changed for [`STALL_CEILING`].
/// The subscription opens before the first read, so a change committed
/// between two reads is never missed; `read` must change its answer only
/// together with a change to `collection` that `reader` observes: a reader
/// other than the rows' owner sees a child row when its session link is
/// published, which is a later change to the child's own collection.
pub async fn read_when_changed<R, T, L, F>(
    reader: &R,
    collection: &str,
    what: &str,
    mut read: F,
) -> T
where
    R: snapshot_reader::SnapshotReader + ?Sized,
    F: AsyncFnMut() -> Result<T, L>,
    L: std::fmt::Display,
{
    let mut changes = reader.subscribe_changes(collection).await;
    loop {
        let last = match read().await {
            Ok(value) => return value,
            Err(last) => last,
        };
        if tokio::time::timeout(STALL_CEILING, changes.recv())
            .await
            .is_err()
        {
            panic!("{what}: {collection} unchanged for {STALL_CEILING:?}; last: {last}");
        }
    }
}

pub fn attach_node_extensions(
    node: &std::sync::Arc<defra_node::Node>,
    extensions: &crate::node_extension::Extensions,
) {
    crate::node_extension::attach_all(node, extensions).expect("fixture extensions attach once");
}

const PIN_FIXED_KEY_HEX: &str = "4cbf8c1186d2fcb70559342fd142650a5ec5938d26a187d87e2c061b530d7be46edb79d5f548207182f7911b55709c9e4b9961c709486e5ce920e306470fe6d6";
pub const PIN_FIXED_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

pub fn pin_fixed_signing_identity(dir: &std::path::Path) -> crate::identity::KeyIdentity {
    let key_bytes: Vec<u8> = (0..PIN_FIXED_KEY_HEX.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&PIN_FIXED_KEY_HEX[offset..offset + 2], 16).unwrap())
        .collect();
    let mut key_file = tempfile::NamedTempFile::new_in(dir).expect("create private fixed key");
    std::io::Write::write_all(&mut key_file, &key_bytes).expect("write fixed pinning key");
    let identity = crate::identity::KeyIdentity::load_existing(key_file.path(), None)
        .expect("load fixed identity");
    assert_eq!(
        crate::identity::NodeIdentity::did(&identity),
        PIN_FIXED_DID,
        "fixed pinning key derives a stable DID"
    );
    identity
}

pub fn fixture_root() -> crate::identity::KeyIdentity {
    let home = tempfile::tempdir().unwrap();
    pin_fixed_signing_identity(home.path())
}

pub const FIXTURE_AGENT_NONCE: &str =
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

pub fn fixture_agent(creation_nonce: &str) -> std::sync::Arc<dyn crate::identity::NodeIdentity> {
    let root = fixture_root();
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: crate::identity::derive_agent_identity(&root, creation_nonce)
            .unwrap()
            .did()
            .to_owned(),
        creation_nonce: creation_nonce.to_owned(),
    };
    crate::identity::resolve_agent_identity(&root, &binding).unwrap()
}

pub fn fixture_agent_did(creation_nonce: &str) -> String {
    fixture_agent(creation_nonce).did().to_owned()
}

pub fn fixture_agent_access(
    node: &std::sync::Arc<defra_node::Node>,
    creation_nonce: &str,
) -> crate::config_client::ConfigAccess {
    let agent = fixture_agent(creation_nonce);
    crate::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}

pub fn test_identity(name: &str) -> crate::identity::KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    crate::identity::KeyIdentity::load_or_create(path, None).unwrap()
}

/// Grants the user the `OperatorUser` set as the runtime admin. Without it,
/// seeding the user's client-signed Node and Agent ends in an incomplete
/// signing conversation.
pub async fn grant_operator_user(
    node: &std::sync::Arc<defra_node::Node>,
    runtime: &dyn crate::identity::NodeIdentity,
    user: &crate::identity::KeyIdentity,
) {
    use crate::identity::NodeIdentity as _;
    let admin = crate::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did()).unwrap(),
    );
    crate::identity::GrantSet::OperatorUser
        .grant(&admin, &identity::Did::new(user.did()).unwrap())
        .await
        .unwrap();
}

fn home_status_body(
    node_did: &str,
    reports: &gents_protocol::delegation_audience::ReportedDelegationAudience,
    provider_time: Option<u64>,
) -> serde_json::Value {
    use gents_protocol::client_authentication::ClientAuthentication;
    use gents_protocol::delegation_audience::ReportedDelegationAudience;
    let authentication = match reports {
        ReportedDelegationAudience::Named(audience) => ClientAuthentication::Vera {
            worker: audience.worker.clone(),
            deployment: audience.deployment,
        },
        ReportedDelegationAudience::Absent => ClientAuthentication::Local,
        ReportedDelegationAudience::Unavailable => ClientAuthentication::Unavailable,
    };
    let mut status = serde_json::json!({
        gents_protocol::delegation_audience::STATUS_NODE_DID_FIELD: node_did,
        gents_protocol::client_authentication::STATUS_FIELD: authentication,
    });
    if let Some(reported) = reports.status_value() {
        status[gents_protocol::delegation_audience::STATUS_DELEGATION_AUDIENCE_FIELD] = reported;
    }
    if let Some(time) = provider_time {
        status[gents_protocol::delegation_audience::STATUS_PROVIDER_TIME_FIELD] = time.into();
    }
    status
}

pub async fn signed_home_status(
    node: &dyn crate::identity::NodeIdentity,
    reports: &gents_protocol::delegation_audience::ReportedDelegationAudience,
    provider_time: Option<u64>,
    nonce: &gents_protocol::home_attestation::HomeNonce,
) -> serde_json::Value {
    let mut status = home_status_body(node.did(), reports, provider_time);
    crate::identity::home_status::attest(Some(node), nonce, &mut status).await;
    status
}

pub fn local_home_status_now(
    node_did: &str,
    sign: &dyn Fn(&[u8]) -> Vec<u8>,
    query: Option<&str>,
) -> serde_json::Value {
    use gents_protocol::home_attestation::{
        HomeAttestation, HomeNonce, HomeStatement, HOME_NONCE_QUERY, STATUS_HOME_ATTESTATION_FIELD,
    };
    let mut status = home_status_body(
        node_did,
        &gents_protocol::delegation_audience::ReportedDelegationAudience::Absent,
        None,
    );
    let nonce = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == HOME_NONCE_QUERY).then(|| HomeNonce::parse(value).ok())?
        });
    if let Some(nonce) = nonce {
        let statement = HomeStatement::of_status(&status, &nonce);
        let signature = sign(&statement.signing_payload());
        status[STATUS_HOME_ATTESTATION_FIELD] =
            serde_json::json!(HomeAttestation::signed(statement, &signature));
    }
    status
}

pub async fn delete_configuration_document(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
) -> Result<(), crate::config_client::ConfigClientError> {
    txn.delete_configuration_document(collection, doc_id).await
}

/// A test's `#[tokio::test]` runtime is its entry point; constructors take
/// that runtime's handle from here.
#[allow(
    clippy::disallowed_methods,
    reason = "a test's #[tokio::test] runtime is its entry-point runtime"
)]
pub fn runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

/// Fixtures that reopen a store must retain its original signing identity.
pub fn node_builder() -> defra_node::NodeBuilder {
    keyed_node_builder().0
}

/// [`node_builder`] with the runtime key it names, for a fixture that builds
/// the host's own access (`ConfigAccess::local_runtime`).
pub fn keyed_node_builder() -> (
    defra_node::NodeBuilder,
    std::sync::Arc<crate::identity::KeyIdentity>,
) {
    use crate::identity::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap();
    (
        defra_node::Node::builder().with_node_identity_did(identity.did()),
        std::sync::Arc::new(identity),
    )
}

pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => std::env::set_var(self.key, previous),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Writes the private key this process holds for `node_did` to `path`.
pub fn write_held_identity_key(node_did: &str, path: &std::path::Path) {
    let config = defra_core::signing::get_identity(node_did)
        .unwrap_or_else(|| panic!("fixture node identity {node_did} is not held in this process"));
    assert!(
        config.has_local_private_key(),
        "fixture node identity {node_did} has no local private key"
    );
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).expect("create held identity key");
    file.write_all(&config.private_key_bytes)
        .expect("write held identity key");
}

/// Recovers the runtime root held by this process for `node_did`. Loading a
/// `KeyIdentity` registers its signing key with DefraDB; Agent derivation
/// needs the held key, which `RegisteredIdentity` never carries.
pub fn held_runtime_identity(node_did: &str) -> crate::identity::KeyIdentity {
    use crate::identity::NodeIdentity as _;
    let dir = tempfile::tempdir().expect("held identity directory");
    let path = dir.path().join("runtime.key");
    write_held_identity_key(node_did, &path);
    let identity = crate::identity::KeyIdentity::load_existing(path.clone(), None)
        .expect("reload held runtime identity");
    assert_eq!(identity.did(), node_did);
    identity
}

pub fn first_content<T>(items: &[T]) -> &T {
    items.first().expect("non-empty content")
}

pub const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
     <function=describe_tool>\", \"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";

/// The verified signer of `cid`, read under document ACP as `reader`: a
/// private row's block is visible only to an actor its policy admits, so an
/// anonymous node read of it fails closed.
pub async fn verified_block_signer_as(node: &defra_node::Node, reader: &str, cid: &str) -> String {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(reader))
        .await
        .unwrap_or_else(|error| panic!("read {cid} as {reader}: {error:#}"));
    let block = defra_core::block::Block::from_dag_cbor(&block).expect("decode signed block");
    let signature =
        defra_core::block::Signature::from_dag_cbor(&signature).expect("decode block signature");
    db::block::verify::verified_signature_signer_did(&block, &signature)
        .unwrap_or_else(|error| panic!("verify {cid}: {error}"))
}

/// Makes write-attempt events visible to a scoped subscriber. tracing caches
/// one interest per callsite for the whole process, and while a single
/// dispatcher is registered it computes that interest from the default of
/// whichever thread reaches the callsite first; a thread with no default
/// caches `never`, which then hides the event from a scoped subscriber on
/// another thread. A global default that enables the target is every such
/// thread's default.
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
        metadata.target() == crate::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET
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

pub fn unbound_journal_schema() -> String {
    use gents_protocol::schemas::{BuiltInPolicy, CLIENT_JOURNAL_POLICY_RESOURCE};
    let directive = format!(
        r#" @policy(id: "{}", resource: "{CLIENT_JOURNAL_POLICY_RESOURCE}")"#,
        BuiltInPolicy::ClientJournal.placeholder()
    );
    let sdl = gents_protocol::schemas::PENDING_CHAT_SUBMISSION;
    assert!(sdl.contains(&directive));
    sdl.replacen(&directive, "", 1)
}

pub async fn client_journal_node(
    client_did: &str,
) -> anyhow::Result<std::sync::Arc<defra_node::Node>> {
    let node = std::sync::Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(client_did)
            .build()
            .await?,
    );
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::node_store().render(&unbound_journal_schema()),
    )
    .await?;
    Ok(node)
}

pub fn local_authentication_route(
    node: std::sync::Arc<dyn crate::identity::NodeIdentity>,
) -> axum::Router {
    axum::Router::new().route(
        "/status",
        axum::routing::get(
            move |query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                let node = node.clone();
                async move {
                    let nonce = query
                        .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
                        .and_then(|nonce| {
                            gents_protocol::home_attestation::HomeNonce::parse(nonce).ok()
                        });
                    axum::Json(match nonce {
                        Some(nonce) => {
                            signed_home_status(
                                node.as_ref(),
                                &gents_protocol::delegation_audience::ReportedDelegationAudience::Absent,
                                None,
                                &nonce,
                            )
                            .await
                        }
                        None => serde_json::json!({
                            "node_did": node.did(),
                            (gents_protocol::client_authentication::STATUS_FIELD):
                                gents_protocol::client_authentication::ClientAuthentication::Local,
                        }),
                    })
                }
            },
        ),
    )
}

pub async fn unrelated_document_reader(
    node: &std::sync::Arc<defra_node::Node>,
) -> crate::config_client::ConfigAccess {
    use crate::identity::NodeIdentity as _;
    let identity = tokio::task::spawn_blocking(|| {
        let directory = tempfile::tempdir().unwrap();
        crate::identity::KeyIdentity::load_or_create(directory.path().join("reader.key"), None)
            .unwrap()
    })
    .await
    .unwrap();
    let actor = identity.did().parse().unwrap();
    crate::config_client::ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    )
    .grant_node_permission(&actor, crate::config_client::NodePermission::DocumentRead)
    .await
    .unwrap();
    crate::config_client::ConfigAccess::local_as(node.clone(), actor)
}
