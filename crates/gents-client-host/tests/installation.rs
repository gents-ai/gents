#![cfg(any(unix, windows))]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use gents_desktop_bridge::host::local;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::error_chain::ErrorChain;
use tokio::process::Command;

mod support;
use support::Attachment;

#[tokio::test]
async fn standalone_installation_shares_one_store_between_stdio_clients_and_releases_it_on_shutdown(
) {
    let dir = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(dir.path().join("client"));
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
        error
            .cause()
            .downcast_ref::<gents::home::HomeError>()
            .and_then(gents::home::HomeError::lock_held)
            .is_some(),
        "{}",
        ErrorChain(&error)
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
