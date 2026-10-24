#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents_desktop_bridge::host::{local, ClientHost};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use crate::ClientHostError;

/// A private bootstrap has no attachment owner until the core reports ready.
/// Dropping its parent must also dismiss an outstanding system authentication
/// request; after readiness the installation owns its ordinary idle lifetime.
struct StartupChild {
    process: Child,
    ready: bool,
}

impl Drop for StartupChild {
    fn drop(&mut self) {
        if !self.ready {
            let _ = self.process.start_kill();
        }
    }
}

pub async fn serve(paths: DesktopPaths, editor_store: bool, interactive: bool) -> Result<()> {
    let provider = editor_store.then(|| {
        Arc::new(crate::editor_store::Provider::new(
            paths.root().to_owned(),
            interactive,
            tokio::io::join(tokio::io::stdin(), tokio::io::stdout()),
        ))
    });
    let options = match &provider {
        Some(provider) => ClientCoreOptions {
            store_key_provider: Some(provider.clone()),
            operator_identity_interaction:
                gents_desktop_core::local_runtime::OperatorIdentityInteraction::Prevent,
            ..Default::default()
        },
        None => ClientCoreOptions::default(),
    };
    let core = match ClientCore::start_with_paths_and_options(paths, options).await {
        Ok(core) => Arc::new(core),
        Err(error) => {
            if let Some(provider) = &provider {
                let _ = provider.failed(error.as_ref()).await;
            }
            return Err(error);
        }
    };
    let listener = match local::LocalListener::bind(core.paths()).await {
        Ok(listener) => listener,
        Err(error) => {
            core.shutdown().await?;
            return Err(error.into());
        }
    };
    if let Some(provider) = &provider {
        if let Err(error) = provider.ready().await {
            core.shutdown().await?;
            return Err(error.into());
        }
    }
    drop(provider);
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
pub async fn attach(paths: DesktopPaths, editor_store: bool, interactive: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        tokio::select! {
            result = attach_inner(paths, editor_store, interactive) => result,
            _ = terminate.recv() => Ok(()),
            _ = interrupt.recv() => Ok(()),
        }
    }
    #[cfg(windows)]
    attach_inner(paths, editor_store, interactive).await
}

async fn attach_inner(paths: DesktopPaths, editor_store: bool, interactive: bool) -> Result<()> {
    let mut bootstrap = editor_store.then(|| {
        crate::editor_store::frames(tokio::io::join(tokio::io::stdin(), tokio::io::stdout()))
    });
    let connection = tokio::time::timeout(Duration::from_secs(90), async {
        if let Ok(connection) = local::connect(&paths).await {
            if let Some(bootstrap) = &mut bootstrap {
                crate::editor_store::send(bootstrap, &crate::editor_store::Message::Ready).await?;
            }
            return Ok(connection);
        }
        let _startup = local::startup_lock(&paths).await?;
        if let Ok(connection) = local::connect(&paths).await {
            if let Some(bootstrap) = &mut bootstrap {
                crate::editor_store::send(bootstrap, &crate::editor_store::Message::Ready).await?;
            }
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
        if editor_store {
            command
                .arg("--editor-store")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            if interactive {
                command.arg("--allow-keychain-prompts");
            }
        }
        #[cfg(unix)]
        command.as_std_mut().process_group(0);
        #[cfg(windows)]
        command.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
                | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
        );
        let mut child = StartupChild {
            process: command
                .spawn()
                .map_err(ClientHostError::StartInstallation)?,
            ready: !editor_store,
        };
        if let Some(bootstrap) = &mut bootstrap {
            use futures::SinkExt;
            let input = child
                .process
                .stdin
                .take()
                .ok_or(ClientHostError::MissingStoreInput)?;
            let output = child
                .process
                .stdout
                .take()
                .ok_or(ClientHostError::MissingStoreOutput)?;
            let mut private = crate::editor_store::frames(tokio::io::join(output, input));
            loop {
                let frame = crate::editor_store::receive(&mut private).await?;
                let message: crate::editor_store::Message = serde_json::from_slice(&frame)
                    .map_err(ClientHostError::InvalidBootstrapMessage)?;
                let ready = matches!(message, crate::editor_store::Message::Ready);
                let failed = matches!(message, crate::editor_store::Message::Error { .. });
                if ready {
                    child.ready = true;
                }
                bootstrap.send(frame.freeze()).await?;
                if failed {
                    return Err(anyhow::Error::from(
                        ClientHostError::LocalHistoryNeedsAttention,
                    ));
                }
                if ready {
                    break;
                }
                let reply = crate::editor_store::receive(bootstrap).await?;
                private.send(reply.freeze()).await?;
            }
        }
        loop {
            if let Ok(connection) = local::connect(&paths).await {
                return Ok(connection);
            }
            if let Some(status) = child.process.try_wait()? {
                return Err(anyhow::Error::from(ClientHostError::HostExited { status }));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(ClientHostError::WaitInstallation)??;
    drop(bootstrap);
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
