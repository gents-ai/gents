use std::path::Path;

use gents_store::test_support::STALL_CEILING;
use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe::{OpenOptions, Receiver};

pub(crate) const HOLD: &str = "printf R > ready && read ignored < gate";

pub(crate) struct HeldProcess {
    ready: Receiver,
}

impl HeldProcess {
    pub(crate) async fn new(directory: &Path) -> Self {
        let status = tokio::process::Command::new("mkfifo")
            .current_dir(directory)
            .args(["ready", "gate"])
            .status()
            .await
            .unwrap();
        assert!(status.success(), "mkfifo exited with {status}");
        let ready = OpenOptions::new()
            .open_receiver(directory.join("ready"))
            .unwrap();
        Self { ready }
    }

    pub(crate) async fn entered(&mut self) {
        let signal = tokio::time::timeout(STALL_CEILING, self.ready.read_u8())
            .await
            .expect("child did not signal readiness")
            .expect("reading child readiness");
        assert_eq!(signal, b'R');
    }
}
