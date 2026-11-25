use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize};

/// A private console delivers Ctrl-C without signalling the runner's console.
/// ConPTY output must remain drained while the console is being closed.
/// portable-pty enables cursor inheritance, which requires replying to position
/// queries before the child can start: https://learn.microsoft.com/windows/console/createpseudoconsole
pub struct ConsoleChild {
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    exited: Arc<AtomicBool>,
    exit_status: Option<ExitStatus>,
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
        let child = pair
            .slave
            .spawn_command(builder)
            .context("starting fixture console")?;
        drop(pair.slave);
        let killer = child.clone_killer();
        Ok(Self {
            child: Some(child),
            killer,
            exited: Arc::new(AtomicBool::new(false)),
            exit_status: None,
            output: Some(output),
        })
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = &self.exit_status {
            return Ok(Some(status.clone()));
        }
        let status = self
            .child
            .as_mut()
            .expect("console child already waited")
            .try_wait()?;
        if let Some(status) = &status {
            self.exited.store(true, Ordering::Release);
            self.exit_status = Some(status.clone());
        }
        Ok(status)
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
        if let Some(status) = &self.exit_status {
            return Ok(status.clone());
        }
        let mut child = self.child.take().context("console child already waited")?;
        let exited = Arc::clone(&self.exited);
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("gents-fixture-console-wait".into())
            .spawn(move || {
                let result = child.wait();
                exited.store(true, Ordering::Release);
                let _ = tx.send(result);
            })?;
        let status = rx.await.context("console waiter thread stopped")??;
        self.exit_status = Some(status.clone());
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
        if !self.exited.load(Ordering::Acquire)
            && !matches!(
                self.child.as_mut().and_then(|child| child.try_wait().ok()),
                Some(Some(_))
            )
        {
            let _ = self.killer.kill();
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
    use gents_desktop_bridge::client_runtime::{ClientRuntime, ClientRuntimeOptions};
    use gents_store::client_journal::paths::DesktopPaths;
    use std::sync::Arc;

    let temp = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(temp.path().join("client"));
    let seed = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await?,
    );
    anyhow::ensure!(seed.node().p2p().is_none());
    let seed_did = seed.node_identity().did().to_owned();
    seed.shutdown().await?;
    drop(seed);
    let mut command = Command::new(env!("CARGO_BIN_EXE_gents-client-host"));
    command
        .envs(super::support::homes::ProcessHomes::under(temp.path()).env())
        .args(["--serve", "--home"])
        .arg(paths.root());
    let log = temp.path().join("console.log");
    let mut child = ConsoleChild::spawn(&command, &log)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        super::support::wait_for_listener(&paths, || {
            anyhow::ensure!(child.try_wait()?.is_none(), "console fixture exited");
            Ok(())
        }),
    )
    .await??;
    let mut attachment = super::support::Attachment::start(&paths).await;
    anyhow::ensure!(attachment.snapshot(1).await["client"]["clientDid"] == seed_did);
    attachment.close().await;
    child.interrupt()?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    anyhow::ensure!(status.success(), "console fixture exited with {status}");
    let reopened = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths,
            ClientRuntimeOptions {
                store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
        )
        .await?,
    );
    anyhow::ensure!(reopened.node().p2p().is_none());
    reopened.shutdown().await?;
    Ok(())
}
