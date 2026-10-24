use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize};

/// A private console delivers Ctrl-C without signalling the runner's console.
/// ConPTY output must remain drained while the console is being closed.
/// portable-pty enables cursor inheritance, which requires replying to position
/// queries before the child can start: https://learn.microsoft.com/windows/console/createpseudoconsole
pub struct ConsoleChild {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    output: Option<ConsoleOutput>,
}

/// The drain may borrow input for cursor replies, but must not keep its handle
/// alive after this owner closes input and starts ConPTY cleanup.
struct ConsoleOutput {
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    master: Option<Box<dyn MasterPty + Send>>,
    drain: Option<JoinHandle<io::Result<u64>>>,
}

impl ConsoleChild {
    pub fn spawn(command: &Command, log: &Path) -> Result<Self> {
        let mut builder = CommandBuilder::new(command.get_program());
        builder.args(command.get_args());
        if let Some(directory) = command.get_current_dir() {
            builder.cwd(directory.as_os_str());
        }
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => builder.env(key, value),
                None => builder.env_remove(key),
            }
        }
        let mut log = File::create(log)?;
        let pair = portable_pty::native_pty_system().openpty(PtySize::default())?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
        let input = Arc::downgrade(&writer);
        let drain = std::thread::Builder::new()
            .name("gents-fixture-console".into())
            .spawn(move || {
                super::console_io::drain(&mut reader, &mut log, || {
                    let Some(input) = input.upgrade() else {
                        return Ok(());
                    };
                    let mut input = input
                        .lock()
                        .map_err(|_| io::Error::other("fixture console input lock poisoned"))?;
                    input.write_all(b"\x1b[1;1R")?;
                    input.flush()
                })
            })?;
        let output = ConsoleOutput {
            writer: Some(writer),
            master: Some(pair.master),
            drain: Some(drain),
        };
        let child = pair.slave.spawn_command(builder);
        drop(pair.slave);
        Ok(Self {
            child: child.context("starting fixture console")?,
            output: Some(output),
        })
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn interrupt(&mut self) -> Result<()> {
        let writer = self
            .output
            .as_mut()
            .and_then(|output| output.writer.as_ref())
            .context("fixture console has already closed")?;
        let mut writer = writer
            .lock()
            .map_err(|_| anyhow::anyhow!("fixture console input lock poisoned"))?;
        writer.write_all(b"\x03")?;
        writer.flush()?;
        Ok(())
    }

    pub async fn wait_for_exit(&mut self) -> Result<ExitStatus> {
        let status = loop {
            if let Some(status) = self.try_wait()? {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        Ok(status)
    }

    pub async fn finish_output(&mut self) -> Result<()> {
        if let Some(mut output) = self.output.take() {
            tokio::task::spawn_blocking(move || output.finish()).await??;
        }
        Ok(())
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        let status = self.wait_for_exit().await?;
        self.finish_output().await?;
        Ok(status)
    }
}

impl Drop for ConsoleChild {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
    }
}

impl ConsoleOutput {
    fn finish(&mut self) -> Result<()> {
        drop(self.writer.take());
        drop(self.master.take());
        if let Some(drain) = self.drain.take() {
            match drain
                .join()
                .map_err(|_| anyhow::anyhow!("fixture console output thread panicked"))?
            {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
                Err(error) => return Err(error).context("draining fixture console output"),
            }
        }
        Ok(())
    }
}

impl Drop for ConsoleOutput {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn console_interrupt_releases_the_native_installation() -> Result<()> {
    use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    use std::sync::Arc;

    let temp = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(temp.path().join("client"));
    let seed = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await?,
    );
    let peer = seed.local_peer_id().to_owned();
    seed.shutdown().await?;
    drop(seed);
    let mut command = Command::new(env!("CARGO_BIN_EXE_gents-client-host"));
    command.args(["--serve", "--home"]).arg(paths.root());
    let log = temp.path().join("console.log");
    let mut child = ConsoleChild::spawn(&command, &log)?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if gents_desktop_bridge::host::local::connect(&paths)
                .await
                .is_ok()
            {
                break;
            }
            anyhow::ensure!(child.try_wait()?.is_none(), "console fixture exited");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let mut attachment = super::support::Attachment::start(&paths).await;
    anyhow::ensure!(attachment.snapshot(1).await["client"]["localPeerId"] == peer);
    attachment.close().await;
    child.interrupt()?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    anyhow::ensure!(status.success(), "console fixture exited with {status}");
    let reopened = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?,
    );
    anyhow::ensure!(reopened.local_peer_id() == peer);
    reopened.shutdown().await?;
    Ok(())
}
