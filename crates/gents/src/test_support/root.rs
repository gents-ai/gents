use super::documents::{bootstrap_signed_test_node, set_test_default_agent_as};
use super::inference::install_test_agent_binding_with_access;

const CAPTURED_EVENT_TARGETS: &[&str] = &[
    crate::store::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET,
    crate::runtime::status::RECONCILE_PHASE_EVENT_TARGET,
    crate::session::REFUSAL_PROJECTION_EVENT_TARGET,
];

pub(crate) fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(CapturedTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

struct CapturedTargetSubscriber;

impl tracing::Subscriber for CapturedTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        CAPTURED_EVENT_TARGETS.contains(&metadata.target())
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

pub(crate) async fn signed_test_node(
    name: &str,
) -> (
    crate::support::TestDb,
    crate::store::identity::KeyIdentity,
    crate::store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let db = crate::support::test_db(name).await;
    signed_test_node_from_db(db, name).await
}

pub(crate) async fn signed_test_node_without_agent(
    name: &str,
) -> (
    crate::support::TestDb,
    crate::store::identity::KeyIdentity,
    crate::store::config_client::ConfigAccess,
) {
    let db = crate::support::test_db(name).await;
    signed_test_node_without_agent_from_db(db, name).await
}

pub(crate) async fn signed_test_node_without_agent_from_db(
    db: crate::support::TestDb,
    name: &str,
) -> (
    crate::support::TestDb,
    crate::store::identity::KeyIdentity,
    crate::store::config_client::ConfigAccess,
) {
    use crate::store::config_client::ConfigAccess;
    use crate::store::identity::{GrantSet, NodeIdentity};

    let fixture_name = name.to_owned();
    let user = tokio::task::spawn_blocking(move || {
        crate::store::test_support::test_identity(&fixture_name)
    })
    .await
    .unwrap();
    let runtime = db.node_identity.as_ref();
    assert_ne!(runtime.did(), user.did());
    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did()).unwrap());
    let user_did = identity::Did::new(user.did()).unwrap();
    GrantSet::OperatorUser
        .grant(&admin, &user_did)
        .await
        .unwrap();
    let access = db.client_access(&user);
    bootstrap_signed_test_node(&access, runtime.did()).await;
    (db, user, access)
}

pub async fn signed_test_node_from_db(
    db: crate::support::TestDb,
    name: &str,
) -> (
    crate::support::TestDb,
    crate::store::identity::KeyIdentity,
    crate::store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use crate::store::identity::NodeIdentity;

    let (db, user, access) = signed_test_node_without_agent_from_db(db, name).await;
    let runtime = db.node_identity.as_ref();
    let user_did = identity::Did::new(user.did()).unwrap();
    let binding =
        crate::store::identity::provision_agent_identity_authorized(&db.node, runtime, &user_did)
            .await
            .unwrap();
    install_test_agent_binding_with_access(
        runtime.did(),
        "Default fixture agent",
        &binding,
        &access,
    )
    .await;
    set_test_default_agent_as(&access, runtime.did(), &binding.agent_did).await;
    (db, user, access, binding)
}
