#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

struct Peer {
    child: Child,
    input: FramedWrite<ChildStdin, LengthDelimitedCodec>,
    output: FramedRead<ChildStdout, LengthDelimitedCodec>,
}

impl Peer {
    fn spawn(directory: &std::path::Path, home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
            .arg("--editor-checkpoints")
            .arg(directory)
            .env("GENTS_HOME", home)
            .env("GENTS_DESKTOP_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = FramedWrite::new(child.stdin.take().unwrap(), LengthDelimitedCodec::new());
        let output = FramedRead::new(child.stdout.take().unwrap(), LengthDelimitedCodec::new());
        Self {
            child,
            input,
            output,
        }
    }

    async fn receive(&mut self) -> Value {
        let bytes = tokio::time::timeout(Duration::from_secs(10), self.output.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn call(&mut self, value: Value) -> Value {
        self.input
            .send(Bytes::from(serde_json::to_vec(&value).unwrap()))
            .await
            .unwrap();
        self.receive().await
    }

    async fn close(&mut self, id: u64) {
        assert_eq!(
            self.call(json!({"version":1,"id":id,"command":"close"}))
                .await["kind"],
            "closed"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(10), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

fn pending(save: bool) -> Value {
    json!([{"id":"operation","sequence":{"items":[{
        "reference":"retained-reference","actions":["physical-action"],"uri":"file:///project/file",
        "restore":if save {"applied"} else {"pending"},
        "save":{"state":if save {"saving"} else {"pending"},"exact":false}}],"closed":false}}])
}

#[tokio::test]
async fn checkpoint_process_holds_os_guard_and_retains_pending_markers_after_actual_kill() {
    for save in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        let unopened = root.path().join("unopened-client-home");
        let mut first = Peer::spawn(&directory, &unopened);
        assert_eq!(first.receive().await["kind"], "acquired");
        assert_eq!(
            first
                .call(json!({"version":1,"id":1,"command":"read"}))
                .await["kind"],
            "absent"
        );
        let ledger = pending(save);
        assert_eq!(
            first
                .call(json!({"version":1,"id":2,"command":"write","ledger":ledger}))
                .await["kind"],
            "durable"
        );
        let exact = std::fs::read(directory.join("editor-undo.v1.json")).unwrap();
        let mut blocked = Peer::spawn(&directory, &unopened);
        assert_eq!(blocked.receive().await["kind"], "busy");
        blocked.child.wait().await.unwrap();
        first.child.kill().await.unwrap();
        first.child.wait().await.unwrap();
        let mut next = Peer::spawn(&directory, &unopened);
        assert_eq!(next.receive().await["kind"], "acquired");
        let observed = next
            .call(json!({"version":1,"id":1,"command":"read"}))
            .await;
        assert_eq!(observed["kind"], "loaded");
        assert_eq!(observed["ledger"], ledger);
        assert!(first
            .input
            .send(Bytes::from_static(b"late dead-connection write"))
            .await
            .is_err());
        assert_eq!(
            std::fs::read(directory.join("editor-undo.v1.json")).unwrap(),
            exact
        );
        next.close(2).await;
        assert!(!unopened.exists());
    }
}

#[tokio::test]
async fn checkpoint_process_rejects_bootstrap_fields_and_never_initializes_client_state() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("checkpoints");
    let unopened = root.path().join("unopened-client-home");
    let mut peer = Peer::spawn(&directory, &unopened);
    assert_eq!(peer.receive().await["kind"], "acquired");
    assert_eq!(
        peer.call(json!({"version":1,"id":1,"command":"read","home":"forbidden"}))
            .await["kind"],
        "error"
    );
    assert!(!peer.child.wait().await.unwrap().success());
    assert!(!unopened.exists());
    assert!(!directory.join("editor-undo.v1.json").exists());
    let status = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .arg("--editor-checkpoints")
        .arg(&directory)
        .arg("--home")
        .arg(&unopened)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(!status.success());
    assert!(!unopened.exists());
}

#[tokio::test]
async fn checkpoint_process_requires_read_and_monotonic_exact_version_before_writing() {
    for request in [
        json!({"version":1,"id":1,"command":"write","ledger":[]}),
        json!({"version":2,"id":1,"command":"read"}),
        json!({"version":1,"id":0,"command":"read"}),
    ] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        let mut peer = Peer::spawn(&directory, &root.path().join("unopened"));
        assert_eq!(peer.receive().await["kind"], "acquired");
        assert_eq!(peer.call(request).await["kind"], "error");
        assert!(!peer.child.wait().await.unwrap().success());
        assert!(!directory.join("editor-undo.v1.json").exists());
    }
}
