pub type TestError = Box<dyn std::error::Error + Send + Sync>;
pub type TestResult<T = ()> = Result<T, TestError>;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents_desktop_bridge::host::protocol;
use gents_desktop_bridge::BridgeError;
use gents_desktop_core::client::DesktopPaths;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

pub struct Attachment {
    child: Child,
    input: FramedWrite<ChildStdin, LengthDelimitedCodec>,
    output: FramedRead<ChildStdout, LengthDelimitedCodec>,
}

pub fn native_binary(name: &str) -> TestResult<PathBuf> {
    let directory = match std::env::var_os("GENTS_NATIVE_TEST_BIN_DIR") {
        Some(directory) => PathBuf::from(directory),
        None => Path::new(env!("CARGO_BIN_EXE_gents-client-host"))
            .parent()
            .ok_or("native binary directory")?
            .to_owned(),
    };
    assert!(
        directory.is_absolute(),
        "GENTS_NATIVE_TEST_BIN_DIR must be absolute"
    );
    let binary = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.is_file(),
        "native test binary is missing: {}",
        binary.display()
    );
    Ok(binary)
}

impl Attachment {
    pub async fn start(paths: &DesktopPaths) -> Self {
        let mut child = Command::new(native_binary("gents-client-host").unwrap())
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

    pub async fn send(&mut self, message: Value) {
        self.input
            .send(Bytes::from(serde_json::to_vec(&message).unwrap()))
            .await
            .unwrap();
    }

    pub async fn next(&mut self) -> Value {
        self.next_before(tokio::time::Instant::now() + Duration::from_secs(30))
            .await
    }

    async fn next_before(&mut self, deadline: tokio::time::Instant) -> Value {
        let bytes = tokio::time::timeout_at(deadline, self.output.next())
            .await
            .expect("native reply deadline")
            .expect("native output closed")
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    pub async fn call(&mut self, id: u32, name: &str, args: Value) -> Value {
        self.try_call(id, name, args)
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    pub async fn try_call(
        &mut self,
        id: u32,
        name: &str,
        args: Value,
    ) -> Result<Value, BridgeError> {
        self.try_call_with_timeout(id, name, args, Duration::from_secs(30))
            .await
    }

    pub async fn try_call_with_timeout(
        &mut self,
        id: u32,
        name: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        let deadline = tokio::time::Instant::now() + timeout;
        self.send(json!({"kind":"call", "id":id, "command":{"name":name, "args":args}}))
            .await;
        loop {
            let response = self.next_before(deadline).await;
            if response["kind"] == "response" {
                assert_eq!(response["id"], id);
                if !response["error"].is_null() {
                    return Err(serde_json::from_value(response["error"].clone())
                        .expect("typed bridge error"));
                }
                return Ok(response["result"].clone());
            }
        }
    }

    pub async fn snapshot(&mut self, id: u32) -> Value {
        self.call(id, "desktop_client_snapshot", json!({})).await
    }

    pub async fn close(mut self) {
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
