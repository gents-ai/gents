#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents_desktop_bridge::host::{local, ClientHost};
use gents_desktop_core::client::{ClientCore, DesktopPaths};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub async fn serve(paths: DesktopPaths) -> Result<()> {
    let listener = local::LocalListener::bind(&paths).await?;
    let core = Arc::new(ClientCore::start_with_paths(paths).await?);
    let stop = CancellationToken::new();
    let interrupt = stop.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            interrupt.cancel();
        }
    });
    let result = listener
        .serve_until_idle(Arc::new(ClientHost::new(core.clone())), stop)
        .await;
    signal.abort();
    let _ = signal.await;
    let closed = core.shutdown().await;
    result.and(closed)
}

/// Windows named pipes cannot close only their write side. Stdio EOF must
/// release both pipe handles so the installation can retire the attachment.
pub async fn attach(paths: DesktopPaths) -> Result<()> {
    let connection = tokio::time::timeout(Duration::from_secs(90), async {
        if let Ok(connection) = local::connect(&paths).await {
            return Ok(connection);
        }
        let _startup = local::startup_lock(&paths).await?;
        if let Ok(connection) = local::connect(&paths).await {
            return Ok(connection);
        }
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--serve")
            .arg("--home")
            .arg(paths.root())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        command.as_std_mut().process_group(0);
        #[cfg(windows)]
        command.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
                | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
        );
        let mut child = command
            .spawn()
            .context("starting the native client installation")?;
        loop {
            if let Ok(connection) = local::connect(&paths).await {
                return Ok(connection);
            }
            if let Some(status) = child.try_wait()? {
                bail!("client host exited with {status}; inspect the Gents output for the startup error");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("waiting for the native client installation")??;
    let (mut reader, mut writer) = tokio::io::split(connection);
    let input = async {
        tokio::io::copy(&mut tokio::io::stdin(), &mut writer).await?;
        writer.shutdown().await
    };
    let output = async {
        tokio::io::copy(&mut reader, &mut tokio::io::stdout()).await?;
        Result::<(), std::io::Error>::Ok(())
    };
    #[cfg(unix)]
    tokio::try_join!(input, output)?;
    #[cfg(windows)]
    tokio::select! {
        result = input => result?,
        result = output => result?,
    }
    Ok(())
}
