use std::path::{Path, PathBuf};

use gents_store::test_support::STALL_CEILING;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe::{OpenOptions, Receiver};

#[cfg(test)]
pub(crate) const HOLD: &str = "printf R > ready && read ignored < gate";

pub struct HeldProcess {
    ready: Receiver,
    input: PathBuf,
    writer: Option<tokio::fs::File>,
}

impl HeldProcess {
    pub async fn new(directory: &Path) -> Self {
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
        Self {
            ready,
            input: directory.join("gate"),
            writer: None,
        }
    }

    pub fn input(&self) -> &Path {
        &self.input
    }

    pub async fn release_line(&mut self) {
        if self.writer.is_some() {
            return;
        }
        let mut writer = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.input)
            .await
            .unwrap();
        writer.write_all(b"settled\n").await.unwrap();
        writer.flush().await.unwrap();
        self.writer = Some(writer);
    }

    pub async fn entered(&mut self) {
        let signal = tokio::time::timeout(STALL_CEILING, self.ready.read_u8())
            .await
            .expect("child did not signal readiness")
            .expect("reading child readiness");
        assert_eq!(signal, b'R');
    }
}
