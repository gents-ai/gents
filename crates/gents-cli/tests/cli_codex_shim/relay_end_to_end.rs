use super::*;

#[path = "../../src/commands/codex/relay.rs"]
#[allow(dead_code)]
mod relay;

use relay::AgentRelay;

/// A relay that stops forwarding would otherwise hang the handshake forever.
const HANDSHAKE: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_through_the_relay_runs_as_its_agent() -> Result<()> {
    let shim = AgentShim::start(|_| vec![], &[]).await?;
    let other = shim.add_agent("other").await?;
    let relay = AgentRelay::bind(format!("127.0.0.1:{}", shim.shim_port), &other).await?;

    let (mut ws, _) = tokio::time::timeout(HANDSHAKE, connect_async(relay.remote())).await??;
    initialize_config_and_thread(&mut ws, &shim.home_dir).await?;
    assert!(listed_thread_ids(&mut ws).await?.is_empty());

    let thread = start_thread(&mut ws, &shim.home_dir).await?;
    let prompt = format!("hello {}", Uuid::new_v4());
    send_turn(&mut ws, &thread, &prompt).await?;
    read_turn_to_completion(&mut ws).await?;

    let (_, session, agent) = wait_for_request(&shim.graphql, &shim.node_did, &prompt).await?;
    assert_eq!(session, thread);
    assert_eq!(agent, other);
    assert_eq!(listed_thread_ids(&mut ws).await?, [thread]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_relay_adds_no_authority() -> Result<()> {
    let shim = AgentShim::start(
        |_| {
            vec![
                "--codex-shim-auth-token-env".to_string(),
                "GENTS_SHIM_TEST_TOKEN".to_string(),
            ]
        },
        &[("GENTS_SHIM_TEST_TOKEN", "correct-secret")],
    )
    .await?;
    shim.wait_until_runnable(&shim.default_agent, Some("correct-secret"))
        .await?;
    let relay =
        AgentRelay::bind(format!("127.0.0.1:{}", shim.shim_port), &shim.default_agent).await?;

    let refused = tokio::time::timeout(HANDSHAKE, connect_async(relay.remote()))
        .await?
        .expect_err("the relay must not supply a token");
    super::server_lifecycle::assert_http_status(refused, StatusCode::UNAUTHORIZED)?;

    let mut request = relay.remote().into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer correct-secret"),
    );
    tokio::time::timeout(HANDSHAKE, connect_async(request)).await??;
    Ok(())
}
