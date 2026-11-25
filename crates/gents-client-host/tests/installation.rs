use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use gents_desktop_bridge::client_runtime::{ClientRuntime, ClientRuntimeOptions};
use gents_desktop_bridge::host::local;
use gents_store::client_journal::paths::DesktopPaths;
use tokio::process::Command;

use crate::support;
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
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    assert!(seed.node().p2p().is_none());
    let seed_did = seed.node_identity().did().to_owned();
    seed.shutdown().await.unwrap();
    drop(seed);
    let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .envs(support::homes::ProcessHomes::under(paths.root().parent().unwrap()).env())
        .arg("--serve")
        .arg("--home")
        .arg(paths.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        support::wait_for_listener(&paths, || {
            if let Some(status) = server.try_wait()? {
                panic!(
                    "native installation exited with {status}: {}",
                    std::fs::read_to_string(dir.path().join("server.log")).unwrap()
                );
            }
            Ok(())
        }),
    )
    .await
    .expect("native installation startup")
    .unwrap();
    let mut first = Attachment::start(&paths).await;
    let mut second = Attachment::start(&paths).await;
    assert_eq!(first.snapshot(1).await["client"]["clientDid"], seed_did);
    assert_eq!(second.snapshot(1).await["client"]["clientDid"], seed_did);
    let error = match ClientRuntime::start_with_paths_and_options(
        paths.clone(),
        ClientRuntimeOptions {
            store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        },
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
        gents_store::local::home::store_lock_held(error.as_ref()).is_some(),
        "{error:#}"
    );
    first.close().await;
    assert_eq!(second.snapshot(2).await["client"]["clientDid"], seed_did);
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
        ClientRuntime::start_with_paths_and_options(
            paths,
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(reopened.node_identity().did(), seed_did);
    assert!(reopened.node().p2p().is_none());
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
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    assert!(core.node().p2p().is_none());
    let client_did = core.node_identity().did().to_owned();
    support::retain_journal_input(&core).await;
    let listener = local::LocalListener::bind(&paths).await.unwrap();
    let mut previous_commands = protocol::ClientCommand::NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    previous_commands.retain(|name| name != protocol::ClientCommand::NAMES[0]);
    let host = Arc::new(ClientHost::for_installation_with_command_inventory(
        core,
        "previous-build".into(),
        previous_commands,
    ));
    let old = tokio::spawn(async move {
        listener
            .serve(Arc::clone(&host), CancellationToken::new())
            .await
            .unwrap();
        // As `native::serve`: the retirement is answered once the store is released.
        host.close_and_acknowledge_retirement().await.unwrap();
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
        gents_store::local::home::wait_for_store_release(paths.root(), paths.node_data_dir())
            .await
            .unwrap();
    })
    .await
    .unwrap();
    let reopened = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths,
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    assert!(reopened.node().p2p().is_none());
    assert_eq!(reopened.node_identity().did(), client_did);
    support::assert_retained_journal_input(&reopened).await;
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
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
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
        attachment.snapshot(1).await["client"]["clientDid"],
        core.node_identity().did()
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
