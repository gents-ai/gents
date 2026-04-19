use super::*;

#[test]
fn bootstrap_probe_logs_multi_agent_replicator_state() -> Result<()> {
    init_test_tracing();

    let mock_endpoint = MockModelEndpoint::start("default")?;
    let fixture = build_named_multi_agent_desktop_fixture_with_backend(
        "bootstrap-probe",
        &["alpha", "bravo"],
        &AgentBackendConfig::mock(mock_endpoint.endpoint()),
        global_log_store(),
    )?;

    let desktop = fixture
        .driver
        .app
        .client
        .as_ref()
        .ok_or_else(|| anyhow!("desktop client missing"))?;

    let interesting = [
        defra_agent_protocol::schemas::AGENT_CONVERSATION_NAME,
        defra_agent_protocol::schemas::AGENT_SESSION_NAME,
        defra_agent_protocol::schemas::AGENT_REQUEST_NAME,
        defra_agent_protocol::schemas::AGENT_RESPONSE_NAME,
        defra_agent_protocol::schemas::AGENT_RUNTIME_NAME,
    ];

    eprintln!("desktop peer id: {}", desktop.local_peer_id());
    eprintln!(
        "desktop connected peers: {:?}",
        fixture.runtime.block_on(desktop.p2p().connected_peers())?
    );
    eprintln!(
        "desktop replicators: {:?}",
        fixture.runtime.block_on(desktop.p2p().get_replicators())?
    );
    for name in interesting {
        let collection = desktop
            .node()
            .get_collection(name)?
            .ok_or_else(|| anyhow!("desktop missing collection {name}"))?;
        eprintln!("desktop collection {name} => {}", collection.collection_id);
    }

    for deployment in &fixture.deployments {
        eprintln!(
            "remote {} peer id: {}",
            deployment.label,
            deployment.core.local_peer_id()
        );
        eprintln!(
            "remote {} connected peers: {:?}",
            deployment.label,
            fixture
                .runtime
                .block_on(deployment.core.p2p().connected_peers())?
        );
        eprintln!(
            "remote {} replicators: {:?}",
            deployment.label,
            fixture
                .runtime
                .block_on(deployment.core.p2p().get_replicators())?
        );
        for name in interesting {
            let collection = deployment
                .core
                .node()
                .get_collection(name)?
                .ok_or_else(|| anyhow!("{} missing collection {name}", deployment.label))?;
            eprintln!(
                "remote {} collection {} => {}",
                deployment.label, name, collection.collection_id
            );
        }
    }

    fixture.shutdown()
}
