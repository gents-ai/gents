#![cfg(any(unix, windows))]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use gents_desktop_bridge::host::local;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use tokio::process::Command;

mod support;
use support::Attachment;

#[tokio::test]
async fn standalone_installation_shares_one_store_between_stdio_clients_and_releases_it_on_shutdown(
) {
    check_shared_installation("client").await;
}

#[cfg(unix)]
#[tokio::test]
async fn long_history_home_supports_shared_stdio_attachments_and_retains_identity() {
    check_shared_installation(&"long-editor-history-location-".repeat(5)).await;
}

async fn check_shared_installation(home: &str) {
    let dir = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(dir.path().join(home));
    let seed = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let peer_id = seed.local_peer_id().to_owned();
    seed.shutdown().await.unwrap();
    drop(seed);
    let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .arg("--serve")
        .arg("--home")
        .arg(paths.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if local::connect(&paths).await.is_ok() {
                break;
            }
            if let Some(status) = server.try_wait().unwrap() {
                panic!(
                    "native installation exited with {status}: {}",
                    std::fs::read_to_string(dir.path().join("server.log")).unwrap()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native installation startup");
    let mut first = Attachment::start(&paths).await;
    let mut second = Attachment::start(&paths).await;
    assert_eq!(first.snapshot(1).await["client"]["localPeerId"], peer_id);
    assert_eq!(second.snapshot(1).await["client"]["localPeerId"], peer_id);
    let error = match ClientCore::start_with_paths_and_options(
        paths.clone(),
        ClientCoreOptions::local_only(),
    )
    .await
    {
        Err(error) => error,
        Ok(core) => {
            Arc::new(core).shutdown().await.unwrap();
            panic!("a second owner opened the installation store");
        }
    };
    assert!(
        gents::store::local::home::store_lock_held(error.as_ref()).is_some(),
        "{error:#}"
    );
    first.close().await;
    assert_eq!(second.snapshot(2).await["client"]["localPeerId"], peer_id);
    second.close().await;
    #[cfg(unix)]
    {
        // SAFETY: this is the live child created above, with a registered SIGINT handler.
        assert_eq!(
            unsafe { libc::kill(server.id().unwrap() as libc::pid_t, libc::SIGINT) },
            0
        );
    }
    let shutdown_wait = if cfg!(windows) {
        Duration::from_secs(210)
    } else {
        Duration::from_secs(30)
    };
    let status = tokio::time::timeout(shutdown_wait, server.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "{}",
        std::fs::read_to_string(dir.path().join("server.log")).unwrap()
    );
    let reopened = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    assert_eq!(reopened.local_peer_id(), peer_id);
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn updated_attachment_retires_the_previous_build_and_preserves_history() {
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use gents_desktop_bridge::host::{protocol, ClientHost};
    use serde_json::{json, Value};
    use tokio_util::codec::LengthDelimitedCodec;
    use tokio_util::sync::CancellationToken;

    let dir = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(dir.path().join("client"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let peer = core.local_peer_id().to_owned();
    let client_did = core.node_identity().did().to_owned();
    let did = gents::graphql::escape_graphql_string(&client_did);
    gents::config_client::ConfigAccess::write_local(
        &core.node_arc(),
        "installation_upgrade.seed",
        &format!(
            r#"mutation {{ create_AgentSession(input: {{session_id: "retained-session",
            node_did: "{did}", requester_did: "{did}", agent_id: "agent",
            created_at: "2026-10-25T00:00:00Z"}}) {{ _docID }} }}"#
        ),
    )
    .await
    .unwrap();
    let listener = local::LocalListener::bind(&paths).await.unwrap();
    let mut previous_commands = protocol::ClientCommand::NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    previous_commands.retain(|name| name != protocol::ClientCommand::NAMES[0]);
    let host = Arc::new(ClientHost::for_installation_with_command_inventory(
        Arc::clone(&core),
        "previous-build".into(),
        previous_commands,
    ));
    let old = tokio::spawn(async move {
        listener
            .serve(host, CancellationToken::new())
            .await
            .unwrap();
        core.shutdown().await.unwrap();
    });
    let mut previous = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(local::connect(&paths).await.unwrap());
    previous
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
                .unwrap(),
        ))
        .await
        .unwrap();
    let hello: Value = serde_json::from_slice(&previous.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(hello["installationBuild"], "previous-build");
    let mut updated = Attachment::start(&paths).await;
    let snapshot = updated.snapshot(1).await;
    assert_eq!(snapshot["client"]["localPeerId"], peer);
    assert_eq!(snapshot["client"]["clientDid"], client_did);
    tokio::time::timeout(Duration::from_secs(30), old)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while previous.next().await.is_some() {}
    })
    .await
    .expect("the previous attachment did not close");
    updated.close().await;

    let mut current = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(local::connect(&paths).await.unwrap());
    current
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
                .unwrap(),
        ))
        .await
        .unwrap();
    let hello: Value = serde_json::from_slice(&current.next().await.unwrap().unwrap()).unwrap();
    assert_ne!(hello["installationBuild"], "previous-build");
    let commands = hello["commands"]
        .as_array()
        .expect("running command inventory");
    for name in protocol::ClientCommand::NAMES {
        assert!(
            commands
                .iter()
                .any(|command| command.as_str() == Some(*name)),
            "running installation does not advertise {name}"
        );
    }
    assert!(commands
        .iter()
        .any(|command| command == "desktop_session_provenance"));
    assert_eq!(
        hello["installationControlVersion"],
        protocol::INSTALLATION_CONTROL_VERSION
    );
    current
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind":"retire", "id":9,
        "expectedBuild":hello["installationBuild"]}))
            .unwrap(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(frame) = current.next().await {
            frame.unwrap();
        }
        loop {
            match gents::store::local::home::lock_store(paths.root(), paths.node_data_dir()) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(error) if error.lock_held().is_some() => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("{error:#}"),
            }
        }
    })
    .await
    .unwrap();
    let reopened = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    assert_eq!(reopened.local_peer_id(), peer);
    assert_eq!(reopened.node_identity().did(), client_did);
    let rows = gents::config_client::ConfigAccess::local(reopened.node_arc())
        .execute("{ AgentSession { session_id } }")
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["AgentSession"][0]["session_id"],
        "retained-session"
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_build_with_compatible_inventory_is_reused_without_retirement() {
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use gents_desktop_bridge::host::{protocol, ClientHost};
    use serde_json::{json, Value};
    use tokio_util::codec::LengthDelimitedCodec;
    use tokio_util::sync::CancellationToken;

    let dir = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(dir.path().join("client"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let stop = CancellationToken::new();
    let listener = local::LocalListener::bind(&paths).await.unwrap();
    let host = Arc::new(ClientHost::for_installation(
        Arc::clone(&core),
        "different-compatible-build".into(),
    ));
    let server_stop = stop.clone();
    let server_core = Arc::clone(&core);
    let server = tokio::spawn(async move {
        listener.serve(host, server_stop).await.unwrap();
        server_core.shutdown().await.unwrap();
    });

    let mut old_host_observer = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(local::connect(&paths).await.unwrap());
    old_host_observer
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
                .unwrap(),
        ))
        .await
        .unwrap();
    let hello: Value =
        serde_json::from_slice(&old_host_observer.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(hello["installationBuild"], "different-compatible-build");

    let mut attachment = Attachment::start(&paths).await;
    assert_eq!(
        attachment.snapshot(1).await["client"]["localPeerId"],
        core.local_peer_id()
    );
    assert!(
        !server.is_finished(),
        "a compatible different build was retired"
    );
    let hello: Value = {
        let mut current = LengthDelimitedCodec::builder()
            .max_frame_length(protocol::MAX_FRAME_BYTES)
            .new_framed(local::connect(&paths).await.unwrap());
        current
            .send(Bytes::from(
                serde_json::to_vec(&json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
                    .unwrap(),
            ))
            .await
            .unwrap();
        serde_json::from_slice(&current.next().await.unwrap().unwrap()).unwrap()
    };
    assert_eq!(hello["installationBuild"], "different-compatible-build");

    stop.cancel();
    attachment.wait_for_output_eof().await;
    let status = attachment.wait_for_exit().await;
    assert!(status.success(), "stdio attachment exited with {status}");
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("compatible host did not stop after cancellation")
        .unwrap();
    assert!(old_host_observer.next().await.is_none());
}
