use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Helper {
    child: Child,
    responses: mpsc::Receiver<Value>,
}

impl Helper {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gents-media-host"))
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut output = child.stdout.take().unwrap();
        let (send, responses) = mpsc::channel();
        std::thread::spawn(move || loop {
            let mut header = [0; 4];
            if output.read_exact(&mut header).is_err() {
                break;
            }
            let length = u32::from_be_bytes(header) as usize;
            assert!(length <= gents_protocol::client_media::MAX_FRAME_BYTES);
            let mut body = vec![0; length];
            output.read_exact(&mut body).unwrap();
            if send.send(serde_json::from_slice(&body).unwrap()).is_err() {
                break;
            }
        });
        Self { child, responses }
    }

    fn call(&mut self, id: u32, operation: Value) -> Value {
        let bytes = serde_json::to_vec(&json!({"id":id,"operation":operation})).unwrap();
        let input = self.child.stdin.as_mut().unwrap();
        input
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(&bytes).unwrap();
        input.flush().unwrap();
        let response = self.responses.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(response["id"], id);
        response
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous test bounding a child process exit in real time"
    )]
    fn exited(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "media helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn artifact_description_does_not_initialize_a_store_or_audio_device() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("unopened");
    let output = Command::new(env!("CARGO_BIN_EXE_gents-media-host"))
        .arg("--describe")
        .env("GENTS_HOME", &home)
        .env("GENTS_DESKTOP_HOME", &home)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let description: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(description["name"], "gents-media-host");
    assert_eq!(description["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        description["protocolVersion"],
        gents_protocol::client_media::VERSION
    );
    assert_eq!(
        description["maxPcmBytes"],
        gents_protocol::client_media::MAX_PCM_BYTES
    );
    assert!(!home.exists());
}

#[test]
fn protocol_initializes_once_and_rejects_invalid_playback_before_opening_a_device() {
    let mut helper = Helper::start();
    assert!(helper.call(1, json!({"command":"stop"}))["error"]
        .as_str()
        .unwrap()
        .contains("Initialize"));
    assert_eq!(
        helper.call(2, json!({"command":"hello","version":1}))["result"]["version"],
        1
    );
    assert!(helper.call(3, json!({"command":"hello","version":1}))["error"].is_string());
    assert!(helper.call(
        4,
        json!({"command":"play","token":"test","sampleRate":48000,"pcm":"!"})
    )["error"]
        .is_string());
    assert!(
        helper.call(5, json!({"command":"finish","token":"not-recording"}))["error"].is_string()
    );
    assert!(helper.call(6, json!({"command":"stop"}))["error"].is_null());
    drop(helper.child.stdin.take());
    assert!(helper.exited().success());
}

#[test]
fn oversized_frames_exit_without_reading_an_audio_payload() {
    let mut helper = Helper::start();
    helper
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&u32::MAX.to_be_bytes())
        .unwrap();
    assert!(!helper.exited().success());
}
