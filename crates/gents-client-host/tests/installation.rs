#![cfg(unix)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents_desktop_bridge::host::{local, protocol};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::{json, Value};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

struct Attachment {
    child: Child,
    input: FramedWrite<ChildStdin, LengthDelimitedCodec>,
    output: FramedRead<ChildStdout, LengthDelimitedCodec>,
}

impl Attachment {
    async fn start(paths: &DesktopPaths) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
            .arg("--stdio")
            .arg("--home")
            .arg(paths.root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = FramedWrite::new(child.stdin.take().unwrap(), LengthDelimitedCodec::new());
        let output = FramedRead::new(child.stdout.take().unwrap(), LengthDelimitedCodec::new());
        let mut attachment = Self {
            child,
            input,
            output,
        };
        attachment
            .send(json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
            .await;
        let hello = attachment.next().await;
        assert_eq!(hello["kind"], "hello");
        assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
        attachment
    }

    async fn send(&mut self, message: Value) {
        self.input
            .send(Bytes::from(serde_json::to_vec(&message).unwrap()))
            .await
            .unwrap();
    }

    async fn next(&mut self) -> Value {
        let bytes = tokio::time::timeout(Duration::from_secs(30), self.output.next())
            .await
            .expect("native reply deadline")
            .expect("native output closed")
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn snapshot(&mut self, id: u32) -> Value {
        self.send(json!({"kind":"call", "id":id, "command":{"name":"desktop_client_snapshot", "args":{}}})).await;
        loop {
            let response = self.next().await;
            if response["kind"] == "response" {
                assert_eq!(response["id"], id);
                assert!(response["error"].is_null(), "{response}");
                return response["result"].clone();
            }
        }
    }

    async fn close(mut self) {
        self.input.close().await.unwrap();
        drop(self.input);
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(frame) = self.output.next().await {
                frame.unwrap();
            }
        })
        .await
        .expect("native attachment closes its output");
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}

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
    assert!(error.is::<gents::home::StoreLockHeld>(), "{error:#}");
    first.close().await;
    assert_eq!(second.snapshot(2).await["client"]["localPeerId"], peer_id);
    second.close().await;
    // SAFETY: this is the live child created above, with a registered SIGINT handler.
    assert_eq!(
        unsafe { libc::kill(server.id().unwrap() as libc::pid_t, libc::SIGINT) },
        0
    );
    let status = tokio::time::timeout(Duration::from_secs(30), server.wait())
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
