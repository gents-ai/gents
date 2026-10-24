#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents_desktop_bridge::host::{local, ClientHost};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

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
    let build = installation_build().await?;
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
                let _ = provider.failed(&error).await;
            }
            return Err(error);
        }
    };
    let listener = match local::LocalListener::bind(core.paths()).await {
        Ok(listener) => listener,
        Err(error) => {
            core.shutdown().await?;
            return Err(error);
        }
    };
    if let Some(provider) = &provider {
        if let Err(error) = provider.ready().await {
            core.shutdown().await?;
            return Err(error);
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
        .serve_until_idle(
            Arc::new(ClientHost::for_installation(core.clone(), build)),
            stop,
        )
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
    let build = installation_build().await?;
    let connection = tokio::time::timeout(Duration::from_secs(90), async {
        if matches!(inspect_installation(&paths, &build).await?, Installation::Current) {
            let connection = local::connect(&paths).await?;
            if let Some(bootstrap) = &mut bootstrap {
                crate::editor_store::send(bootstrap, &crate::editor_store::Message::Ready).await?;
            }
            return Ok(connection);
        }
        let _startup = local::startup_lock(&paths).await?;
        let observed = inspect_installation(&paths, &build).await?;
        if matches!(observed, Installation::Current) {
            let connection = local::connect(&paths).await?;
            if let Some(bootstrap) = &mut bootstrap {
                crate::editor_store::send(bootstrap, &crate::editor_store::Message::Ready).await?;
            }
            return Ok(connection);
        }
        if let Installation::Outdated { build: previous, control } = observed {
            if control != u64::from(gents_desktop_bridge::host::protocol::INSTALLATION_CONTROL_VERSION) {
                let message = "The application serving this history predates safe updates. Update that application, or close its clients and retry after it exits. Your history is unchanged.";
                if let Some(bootstrap) = &mut bootstrap {
                    crate::editor_store::send(bootstrap, &crate::editor_store::Message::Error {
                        error: gents_desktop_bridge::BridgeError::new(
                            gents_desktop_bridge::BridgeErrorCode::Unsupported, message),
                    }).await?;
                }
                bail!(message);
            }
            retire_installation(&paths, &previous).await?;
            wait_for_store_release(&paths).await?;
            gents_protocol::client_installation::State {
                phase: gents_protocol::client_installation::Phase::Released,
                attachments: 0,
            }.step(gents_protocol::client_installation::Action::Replace { startup_reserved: true })
                .context("client installation replacement was refused")?;
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
            command.arg("--editor-store").stdin(Stdio::piped()).stdout(Stdio::piped());
            if interactive { command.arg("--allow-keychain-prompts"); }
        }
        #[cfg(unix)]
        command.as_std_mut().process_group(0);
        #[cfg(windows)]
        command.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
                | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
        );
        let mut child = StartupChild {
            process: command.spawn().context("starting the native client installation")?,
            ready: !editor_store,
        };
        if let Some(bootstrap) = &mut bootstrap {
            use futures::SinkExt;
            let input = child.process.stdin.take().context("missing private store input")?;
            let output = child.process.stdout.take().context("missing private store output")?;
            let mut private = crate::editor_store::frames(tokio::io::join(output, input));
            loop {
                let frame = crate::editor_store::receive(&mut private).await?;
                let message: crate::editor_store::Message = serde_json::from_slice(&frame)
                    .context("invalid store bootstrap message")?;
                let ready = matches!(message, crate::editor_store::Message::Ready);
                let failed = matches!(message, crate::editor_store::Message::Error { .. });
                if ready { child.ready = true; }
                bootstrap.send(frame.freeze()).await?;
                if failed { bail!("local history needs attention in the editor"); }
                if ready { break; }
                let reply = crate::editor_store::receive(bootstrap).await?;
                private.send(reply.freeze()).await?;
            }
        }
        loop {
            if let Ok(connection) = local::connect(&paths).await {
                return Ok(connection);
            }
            if let Some(status) = child.process.try_wait()? {
                bail!("client host exited with {status}; inspect the Gents output for the startup error");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("waiting for the native client installation")??;
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

#[derive(Debug)]
enum Installation {
    Missing,
    Current,
    Outdated { build: String, control: u64 },
}

async fn installation_build() -> Result<String> {
    tokio::task::spawn_blocking(|| {
        let file = std::fs::File::open(std::env::current_exe()?)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader(file)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .context("reading client installation build identity")?
}

async fn inspect_installation(paths: &DesktopPaths, build: &str) -> Result<Installation> {
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use gents_desktop_bridge::host::protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION};
    let Ok(connection) = local::connect(paths).await else {
        return Ok(Installation::Missing);
    };
    let mut frames = tokio_util::codec::LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_framed(connection);
    frames
        .send(Bytes::from(serde_json::to_vec(&serde_json::json!({
            "kind":"hello", "version":PROTOCOL_VERSION
        }))?))
        .await?;
    let Some(frame) = frames.next().await else {
        return Ok(Installation::Outdated {
            build: String::new(),
            control: 0,
        });
    };
    let hello: serde_json::Value = serde_json::from_slice(&frame?)?;
    let current = hello["kind"] == "hello"
        && hello["version"] == PROTOCOL_VERSION
        && (hello["installationBuild"] == build
            || (hello["installationBuild"].is_null() && hello["installationControlVersion"] == 0))
        && hello["maxFrameBytes"] == MAX_FRAME_BYTES
        && hello["commands"].as_array().is_some_and(|commands| {
            gents_desktop_bridge::host::protocol::ClientCommand::NAMES
                .iter()
                .all(|name| {
                    commands
                        .iter()
                        .any(|command| command.as_str() == Some(*name))
                })
        });
    if current {
        Ok(Installation::Current)
    } else {
        Ok(Installation::Outdated {
            build: hello["installationBuild"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            control: hello["installationControlVersion"]
                .as_u64()
                .unwrap_or_default(),
        })
    }
}

async fn retire_installation(paths: &DesktopPaths, expected: &str) -> Result<()> {
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use gents_desktop_bridge::host::protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION};
    let mut frames = tokio_util::codec::LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_framed(local::connect(paths).await?);
    frames
        .send(Bytes::from(serde_json::to_vec(&serde_json::json!({
            "kind":"hello", "version":PROTOCOL_VERSION
        }))?))
        .await?;
    let hello = frames
        .next()
        .await
        .context("shared client closed before update")??;
    let hello: serde_json::Value = serde_json::from_slice(&hello)?;
    anyhow::ensure!(
        hello["installationBuild"] == expected,
        "The shared client changed before the update. Retry the update."
    );
    frames
        .send(Bytes::from(serde_json::to_vec(&serde_json::json!({
            "kind":"retire", "id":1, "expectedBuild":expected
        }))?))
        .await?;
    let mut acknowledged = false;
    while let Some(frame) = frames.next().await {
        let message: serde_json::Value = serde_json::from_slice(&frame?)?;
        if message["kind"] == "response" && message["id"] == 1 {
            anyhow::ensure!(
                message["error"].is_null(),
                "shared client refused the update"
            );
            acknowledged = true;
        }
    }
    anyhow::ensure!(acknowledged, "shared client update was not confirmed");
    Ok(())
}

async fn wait_for_store_release(paths: &DesktopPaths) -> Result<()> {
    loop {
        match gents::home::lock_store(paths.root(), paths.node_data_dir()) {
            Ok(reservation) => {
                drop(reservation);
                return Ok(());
            }
            Err(error) if error.downcast_ref::<gents::home::StoreLockHeld>().is_some() => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error).context("waiting for the previous client store owner"),
        }
    }
}
