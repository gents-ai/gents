use std::process::Command;

#[test]
fn packaged_host_exposes_the_owned_runtime_cli_without_changing_host_flags() {
    for command in ["init", "server", "prepare-runtime"] {
        let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--home"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .arg("--describe")
        .output()
        .unwrap();
    assert!(output.status.success());
    let description: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(description["localRuntimeCli"], true);
    assert_eq!(
        description["runtimePreparationVersion"],
        gents_protocol::serve_lifecycle::preparation::RUNTIME_PREPARATION_VERSION
    );
    assert_eq!(description["name"], "gents-client-host");
}

#[tokio::test]
async fn packaged_service_pairs_with_the_editor_client_and_retains_provider_setup_on_reload() {
    use gents_protocol::serve_lifecycle::preparation::{
        RuntimePreparationOutcome, RuntimePreparationResult,
    };
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    use serde_json::json;

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("runtime");
    let executable = env!("CARGO_BIN_EXE_gents-client-host");
    let init = Command::new(executable)
        .args(["init", "--store-key-custody", "file", "--home"])
        .arg(&home)
        .args([
            "--node-name",
            "Gents",
            "--engineer",
            "--tool-package",
            "minimal",
        ])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let log_path = root.path().join("runtime.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let spawn_preparer = || {
        tokio::process::Command::new(executable)
            .args(["prepare-runtime", "--home"])
            .arg(&home)
            .args([
                "--http-port",
                &port.to_string(),
                "--http-addr",
                "127.0.0.1",
                "--p2p-port",
                "0",
                "--p2p-bind-addr",
                "127.0.0.1",
                "--no-codex-shim",
                "--no-keychain-prompts",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log.try_clone().unwrap()))
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut service = spawn_preparer();
    let mut contender = spawn_preparer();
    let (first, second) = tokio::join!(
        preparation_receipt(&mut service),
        preparation_receipt(&mut contender)
    );
    let results = [first, second];
    let mut outcomes = Vec::new();
    for result in &results {
        let RuntimePreparationResult::Ready {
            outcome,
            node_did,
            graphql,
            build,
            ..
        } = result
        else {
            panic!(
                "{result:?}\n{}",
                std::fs::read_to_string(&log_path).unwrap()
            );
        };
        assert!(node_did.starts_with("did:"));
        assert_eq!(graphql, &format!("http://127.0.0.1:{port}/api/v0/graphql"));
        assert_eq!(build.len(), 64);
        outcomes.push(*outcome);
    }
    assert!(outcomes.contains(&RuntimePreparationOutcome::Started));
    assert!(outcomes.contains(&RuntimePreparationOutcome::Reused));
    for (child, outcome) in [(&mut service, outcomes[0]), (&mut contender, outcomes[1])] {
        if outcome == RuntimePreparationOutcome::Reused {
            assert!(tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .unwrap()
                .unwrap()
                .success());
        } else {
            assert!(child.try_wait().unwrap().is_none());
        }
    }
    let paths = DesktopPaths::from_root(root.path().join("client"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            paths.clone(),
            ClientCoreOptions {
                operator_identity_interaction:
                    gents_desktop_core::local_runtime::OperatorIdentityInteraction::Prevent,
                ..ClientCoreOptions::local_only()
            },
        )
        .await
        .unwrap(),
    );
    let custody: gents::store_key::StoreEncryption =
        serde_json::from_slice(&std::fs::read(paths.store_encryption_path()).unwrap()).unwrap();
    assert_eq!(custody.custody, gents::store_key::StoreKeyCustody::File);
    assert!(paths.store_key_path().is_file());
    let listener = gents_desktop_bridge::host::local::LocalListener::bind(&paths)
        .await
        .unwrap();
    let installation =
        gents_desktop_bridge::host::ClientHostService::start(Arc::clone(&core), listener);
    let mut client = support::Attachment::start(&paths).await;
    client
        .call(1, "client_connect_local", json!({"nodeHome": home}))
        .await;
    let snapshot = client.snapshot(2).await;
    let deployments = snapshot["client"]["deployments"].as_array().unwrap();
    assert_eq!(deployments.len(), 1, "{snapshot}");
    let node = deployments[0]["nodeDid"].clone();
    assert!(node.as_str().is_some_and(|did| did.starts_with("did:")));
    let state = client
        .call(3, "client_inference_setup_state", json!({"nodeDid":node}))
        .await;
    assert!(
        !state["inferenceBackends"].as_array().unwrap().is_empty(),
        "{state}"
    );
    let catalog = client
        .call(4, "client_inference_setup_catalog", json!({}))
        .await;
    assert!(catalog.to_string().contains("OpenAI"), "{catalog}");
    client.close().await;
    let mut reopened = support::Attachment::start(&paths).await;
    let snapshot = reopened.snapshot(5).await;
    assert_eq!(snapshot["client"]["deployments"][0]["nodeDid"], node);
    reopened.close().await;
    installation.shutdown().await.unwrap();
    core.shutdown().await.unwrap();
    for child in [&mut service, &mut contender] {
        if child.try_wait().unwrap().is_none() {
            child.kill().await.unwrap();
        }
        child.wait().await.unwrap();
    }
}

async fn preparation_receipt(
    child: &mut tokio::process::Child,
) -> gents_protocol::serve_lifecycle::preparation::RuntimePreparationResult {
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(90), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .expect("preparation receipt");
    let report: gents_protocol::serve_lifecycle::preparation::RuntimePreparationReport =
        serde_json::from_str(&frame).unwrap();
    assert_eq!(
        report.version,
        gents_protocol::serve_lifecycle::preparation::RUNTIME_PREPARATION_VERSION
    );
    report.result
}

mod support;
