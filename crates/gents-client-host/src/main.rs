use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use gents_desktop_core::client::DesktopPaths;

#[derive(Parser)]
#[command(version)]
struct Args {
    #[arg(long)]
    home: Option<PathBuf>,
    #[arg(long, hide = true, conflicts_with = "stdio")]
    serve: bool,
    #[arg(long)]
    stdio: bool,
    #[arg(long, hide = true)]
    editor_store: bool,
    #[arg(long, hide = true, requires = "editor_store")]
    allow_keychain_prompts: bool,
    #[arg(long, conflicts_with_all = ["home", "serve", "stdio"])]
    describe: bool,
    #[arg(long, hide = true, conflicts_with_all = ["home", "serve", "stdio", "editor_store", "allow_keychain_prompts", "describe"])]
    editor_checkpoints: Option<PathBuf>,
    #[arg(long, hide = true, conflicts_with_all = ["home", "serve", "stdio", "editor_store", "allow_keychain_prompts", "describe", "editor_checkpoints"])]
    unlock_runtime_keys: Option<PathBuf>,
}

mod editor_checkpoints;
#[cfg(any(unix, windows))]
mod editor_store;
#[cfg(any(unix, windows))]
mod native;

#[cfg(windows)]
mod windows_stdio;

fn main() -> Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| !argument.to_string_lossy().starts_with('-'))
    {
        return gents_server::run_cli_without_keychain_prompts();
    }
    let args = Args::parse();
    if let Some(home) = &args.unlock_runtime_keys {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(gents_server::unlock_runtime_keys(home));
    }
    if let Some(directory) = args.editor_checkpoints {
        return editor_checkpoints::serve(
            &directory,
            std::io::stdin().lock(),
            std::io::stdout().lock(),
        );
    }
    let attachment_process = !args.serve && !args.describe;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    let result = runtime.block_on(client_main(args));
    if attachment_process {
        // Tokio's stdin read cannot be cancelled while editor stdin is open.
        // The server owns request execution and database cleanup; this
        // attachment must exit without joining its remaining input read.
        runtime.shutdown_background();
    }
    result
}

async fn client_main(args: Args) -> Result<()> {
    if args.describe {
        use gents_desktop_bridge::host::protocol;
        let mut output = std::io::stdout().lock();
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({
                "name": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
                "protocolVersion": protocol::PROTOCOL_VERSION,
                "maxFrameBytes": protocol::MAX_FRAME_BYTES,
                "commands": protocol::ClientCommand::NAMES,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "transportSupported": cfg!(any(unix, windows)),
                "localRuntimeCli": true,
                "runtimeKeyUnlock": true,
                "runtimePreparationVersion": gents_protocol::serve_lifecycle::preparation::RUNTIME_PREPARATION_VERSION,
                "editorStoreBootstrapVersion": protocol::EDITOR_STORE_BOOTSTRAP_VERSION,
                "editorCheckpointVersion": editor_checkpoints::VERSION,
                "editorCheckpointSupported": cfg!(unix),
            }),
        )?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    gents_desktop_bridge::logging::init_tracing_with_config(gents_desktop_bridge::TracingConfig {
        filter: None,
        console: true,
    });
    let paths = match args.home {
        Some(home) => {
            anyhow::ensure!(
                home.is_absolute(),
                "client home must be an absolute directory"
            );
            DesktopPaths::from_root(home)
        }
        None => DesktopPaths::discover()?,
    };
    #[cfg(any(unix, windows))]
    return if args.serve {
        native::serve(paths, args.editor_store, args.allow_keychain_prompts).await
    } else {
        native::attach(paths, args.editor_store, args.allow_keychain_prompts).await
    };
    #[cfg(not(any(unix, windows)))]
    {
        let _ = paths;
        anyhow::bail!("the native client transport is not available on this platform")
    }
}
