use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub struct BlockingDrop<T>
where
    T: Send + 'static,
{
    value: Option<T>,
}

impl<T> Deref for BlockingDrop<T>
where
    T: Send + 'static,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().unwrap()
    }
}

impl<T> Drop for BlockingDrop<T>
where
    T: Send + 'static,
{
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            tokio::task::spawn_blocking(move || drop(value));
        }
    }
}

pub async fn tempdir() -> Result<BlockingDrop<tempfile::TempDir>> {
    Ok(BlockingDrop {
        value: Some(tokio::task::spawn_blocking(tempfile::tempdir).await??),
    })
}

pub async fn allocate_port() -> Result<u16> {
    tokio::task::spawn_blocking(crate::support::allocate_port).await?
}

pub fn spawn_cli(home: &Path, args: &[&str]) -> Result<Child> {
    Command::new(crate::support::cli_bin())
        .env("HOME", home)
        .env("RUST_LOG", "error")
        .current_dir(home)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning gents {args:?}"))
}

async fn output(home: &Path, args: &[&str]) -> Result<Output> {
    tokio::time::timeout(
        Duration::from_secs(1200),
        spawn_cli(home, args)?.wait_with_output(),
    )
    .await
    .context("live CLI command deadline")?
    .with_context(|| format!("running gents {args:?}"))
}

pub async fn run_cli_text(home: &Path, args: &[&str]) -> Result<String> {
    let output = output(home, args).await?;
    ensure!(
        output.status.success(),
        "gents {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).context("decoding CLI output")
}

pub async fn run_cli_json(home: &Path, args: &[&str]) -> Result<Value> {
    serde_json::from_str(&run_cli_text(home, args).await?).context("decoding CLI JSON")
}

pub async fn run_init_json(home: &Path, args: &[&str]) -> Result<Value> {
    let mut command = vec!["init", "--store-key-custody", "file"];
    command.extend_from_slice(args);
    run_cli_json(home, &command).await
}

pub async fn run_cli_failure_stderr(home: &Path, args: &[&str]) -> Result<String> {
    let output = output(home, args).await?;
    ensure!(
        !output.status.success(),
        "gents {args:?} unexpectedly succeeded"
    );
    String::from_utf8(output.stderr).context("decoding CLI failure")
}

pub struct LiveServer {
    _child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl LiveServer {
    pub async fn captured_output(&self) -> Result<(String, String)> {
        Ok((
            self.stdout.lock().await.clone(),
            self.stderr.lock().await.clone(),
        ))
    }
}

pub async fn spawn_server_with_ready_json(
    home: &Path,
    port: u16,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(LiveServer, Value)> {
    let mut command = Command::new(crate::support::cli_bin());
    command
        .env("HOME", home)
        .env("RUST_LOG", "error")
        .env_remove("GENTS_SYSTEM_LOG")
        .current_dir(home)
        .args(["server", "--http-port", &port.to_string()])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().context("spawning live CLI server")?;
    let mut stdout = child.stdout.take().context("live server stdout")?;
    let mut stderr = child.stderr.take().context("live server stderr")?;
    let out = Arc::new(Mutex::new(String::new()));
    let err = Arc::new(Mutex::new(String::new()));
    let capture = err.clone();
    tokio::spawn(async move {
        let mut buffer = [0; 4096];
        while let Ok(read) = stderr.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            capture
                .lock()
                .await
                .push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    });
    let readiness = tokio::time::timeout(Duration::from_secs(30), async {
        let mut buffer = [0; 4096];
        loop {
            let read = stdout.read(&mut buffer).await?;
            ensure!(
                read > 0,
                "server exited before readiness: {}",
                err.lock().await.as_str()
            );
            let mut text = out.lock().await;
            text.push_str(&String::from_utf8_lossy(&buffer[..read]));
            if let Some(readiness) = crate::support::process::server_readiness_json(&text) {
                return Ok::<_, anyhow::Error>(readiness);
            }
        }
    })
    .await
    .context("live server readiness deadline")??;
    let capture = out.clone();
    tokio::spawn(async move {
        let mut buffer = [0; 4096];
        while let Ok(read) = stdout.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            capture
                .lock()
                .await
                .push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    });
    let served = crate::support::process::served_home_dir(home, args, envs);
    let home = home.to_owned();
    let envs = envs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        crate::support::graphql::register_served_home(
            &home,
            &served,
            &envs
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
            port,
        )
    })
    .await??;
    Ok((
        LiveServer {
            _child: child,
            stdout: out,
            stderr: err,
        },
        readiness,
    ))
}

pub async fn fifo_writer(path: &Path) -> Result<std::fs::File> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            Ok(std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?)
        }
        #[cfg(not(unix))]
        anyhow::bail!("live steering requires Unix FIFOs: {}", path.display())
    })
    .await?
}

pub fn release_fifo(mut writer: std::fs::File) -> Result<()> {
    use std::io::Write;
    writer.write_all(b"released\n")?;
    Ok(())
}

pub async fn run_desktop_init_json(home: &Path, desktop_home: &Path, label: &str) -> Result<Value> {
    let binary = std::env::var_os("GENTS_LIVE_DESKTOP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(crate::support::cli_bin())
                .with_file_name(format!("gents-desktop{}", std::env::consts::EXE_SUFFIX))
        });
    ensure!(
        tokio::fs::metadata(&binary).await.is_ok_and(|metadata| metadata.is_file()),
        "live desktop fixture requires a prebuilt binary at {}; run cargo build -p gents-desktop --bin gents-desktop --profile fast-dev or set GENTS_LIVE_DESKTOP_BIN",
        binary.display(),
    );
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        Command::new(&binary)
            .env("RUST_LOG", "error")
            .env("GENTS_HOME", home)
            .env("GENTS_DESKTOP_HOME", desktop_home)
            .args(["init", "--node-home"])
            .arg(home)
            .arg("--desktop-home")
            .arg(desktop_home)
            .args(["--label", label, "--json"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("desktop initialization deadline")??;
    ensure!(
        output.status.success(),
        "gents-desktop init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&output.stdout).context("decoding desktop initialization")
}
