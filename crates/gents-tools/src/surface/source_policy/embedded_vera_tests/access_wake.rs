use gents_session::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents_session::request::watcher::{DefraWatcher, Watcher as _};
use gents_store::local::document_acp::DocumentAcp;
use gents_store::test_support::{STALL_CEILING, UNREACHED_DEADLINE};

use super::stored_create::ClientUser;
use crate::test_support::vera_test_home::{Household, PROVIDERS};

/// A client's request reaches the runtime's request watcher on `provider`'s
/// home, the watcher's first scan having finished before the request is
/// submitted and its fallback rescan out of reach.
async fn handed_over(provider: DocumentAcp) -> anyhow::Result<()> {
    let household = Household::open(provider).await?;
    let binding = household.derive_agent()?;
    household.create_agent(&binding).await?;
    let agent =
        gents_store::identity::resolve_agent_identity(household.home.node.as_ref(), &binding)?;
    let mut watcher = DefraWatcher::for_agents(
        household.node.clone(),
        household.node_did.as_str(),
        vec![agent],
    )?
    .with_fallback(UNREACHED_DEADLINE);
    let mut scans = watcher.scans();
    let handed = tokio::spawn(async move { watcher.next_request().await });
    tokio::time::timeout(STALL_CEILING, scans.wait_for(|scans| *scans >= 1)).await??;

    let request_id = uuid::Uuid::new_v4().to_string();
    let path = household.directory.path().join("user.key");
    let raw = tokio::task::spawn_blocking(move || {
        gents_store::identity::load_or_create_file_identity(&path)
    })
    .await??;
    let user = ClientUser(household.user.clone(), raw);
    let access = household.as_user()?;
    let create = build_signed_client_request(
        &access,
        RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.clone(),
                node_did: household.node_did.to_string(),
                requester_did: Some(household.user_did.to_string()),
                agent_did: binding.agent_did.clone(),
                session_id: "access-wake-session".into(),
                content: "hello".into(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                household.user_did.as_str(),
            ),
        ),
        RequestSigner::Identity(&user),
    )
    .await?;
    submit_client_request(&access, &create).await?;
    let request = tokio::time::timeout(STALL_CEILING, handed)
        .await
        .map_err(|_| {
            anyhow::anyhow!("{provider}: the request reached the watcher only by its fallback")
        })??
        .ok_or_else(|| anyhow::anyhow!("the request watcher stopped"))??;
    anyhow::ensure!(
        request.request_id == request_id,
        "the watcher handed over another request: {request:?}"
    );
    household.shutdown().await?;
    Ok(())
}

/// TLA `AccessWake` (`MCAccessWakeGreen`, `MCAccessWakeNodeStore`).
#[tokio::test]
async fn a_client_request_reaches_the_watcher_without_its_fallback_rescan() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        handed_over(provider).await?;
    }
    Ok(())
}

/// TLA `AccessWake` (`MCAccessWakeGreen`; without the access wake,
/// `MCAccessWakeRowOnly`).
#[tokio::test]
async fn a_relationship_change_alone_wakes_a_subscription_on_a_vera_home() -> anyhow::Result<()> {
    let household = Household::open(DocumentAcp::Vera).await?;
    let mut changes =
        gents_store::subscription_source::subscribe_collections(&household.node, ["AgentRequest"])?;
    let reader = household.held_key("reader.key").await?;
    let reader = identity::Did::new(gents_store::identity::NodeIdentity::did(reader.as_ref()))?;
    household
        .as_user()?
        .add_actor_relationship("Node", &household.node_doc, "runtime_reader", &reader)
        .await?;
    loop {
        let batch = tokio::time::timeout(STALL_CEILING, changes.recv())
            .await
            .map_err(|_| anyhow::anyhow!("no access change reached the subscription"))?
            .ok_or_else(|| anyhow::anyhow!("the subscription closed"))?;
        anyhow::ensure!(
            gents_store::subscription_source::access_changed(&batch),
            "nothing wrote the watched collection: {batch:?}"
        );
        if household
            .owed_until(&household.user_did, |_| true)
            .await?
            .is_empty()
        {
            break;
        }
    }
    household.shutdown().await?;
    Ok(())
}
