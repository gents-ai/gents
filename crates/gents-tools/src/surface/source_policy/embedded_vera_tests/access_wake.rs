use std::time::{Duration, Instant};

use gents_session::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents_session::request::watcher::{DefraWatcher, Watcher as _};
use gents_store::local::document_acp::DocumentAcp;

use super::stored_create::ClientUser;
use crate::test_support::vera_test_home::{Household, PROVIDERS};

/// How long after its submission a client's request takes to reach the runtime's
/// request watcher on `provider`'s home, the watcher having started first.
async fn handed_over_after(provider: DocumentAcp) -> anyhow::Result<Duration> {
    let household = Household::open(provider).await?;
    let binding = household.derive_agent()?;
    household.create_agent(&binding).await?;
    let agent =
        gents_store::identity::resolve_agent_identity(household.home.node.as_ref(), &binding)?;
    let mut watcher = DefraWatcher::for_agents(
        household.node.clone(),
        household.node_did.as_str(),
        vec![agent],
    )?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let path = household.directory.path().join("user.key");
    let raw = tokio::task::spawn_blocking(move || {
        gents_store::identity::load_or_create_file_identity(&path)
    })
    .await??;
    let user = ClientUser(household.user.clone(), raw);
    let access = household.as_user()?;
    let handed = tokio::spawn(async move { watcher.next_request().await });
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
    let submitted = Instant::now();
    submit_client_request(&access, &create).await?;
    let request = tokio::time::timeout(gents_store::test_support::STALL_CEILING, handed)
        .await??
        .ok_or_else(|| anyhow::anyhow!("the request watcher stopped"))??;
    let after = submitted.elapsed();
    anyhow::ensure!(
        request.request_id == request_id,
        "the watcher handed over another request: {request:?}"
    );
    tracing::info!(%provider, ?after, "client request handed to the request watcher");
    household.shutdown().await?;
    Ok(after)
}

/// TLA `AccessWake` (`MCAccessWakeGreen`, `MCAccessWakeNodeStore`).
#[tokio::test]
async fn a_client_request_reaches_the_watcher_before_its_fallback_rescan() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let after = handed_over_after(provider).await?;
        assert!(
            after < gents_session::request::watcher::cooldown::GOSSIP_FALLBACK_POLL / 2,
            "{provider}: the request was handed over {after:?} after its submission, at the \
             watcher's fallback rescan rather than when its access settled"
        );
    }
    Ok(())
}
