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
}

mod editor_checkpoints;
#[cfg(any(unix, windows))]
mod editor_store;
#[cfg(any(unix, windows))]
mod native;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(directory) = args.editor_checkpoints {
        return editor_checkpoints::serve(
            &directory,
            std::io::stdin().lock(),
            std::io::stdout().lock(),
        );
    }
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
