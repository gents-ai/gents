use std::io::Write;
use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};

use clap::Parser;
use gents_desktop_core::client::DesktopPaths;
use gents_protocol::error_chain::ErrorChain;

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
}

#[cfg(any(unix, windows))]
mod editor_store;
#[cfg(any(unix, windows))]
mod native;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientHostError {
    #[error("client home must be an absolute directory")]
    HomeNotAbsolute,
    #[cfg(not(any(unix, windows)))]
    #[error("the native client transport is not available on this platform")]
    TransportUnavailable,
    #[error("starting the native client installation")]
    StartInstallation(#[source] std::io::Error),
    #[error("waiting for the native client installation")]
    WaitInstallation(#[source] tokio::time::error::Elapsed),
    #[error("missing private store input")]
    MissingStoreInput,
    #[error("missing private store output")]
    MissingStoreOutput,
    #[error("invalid store bootstrap message")]
    InvalidBootstrapMessage(#[source] serde_json::Error),
    #[error("local history needs attention in the editor")]
    LocalHistoryNeedsAttention,
    #[error("client host exited with {status}; inspect the Gents output for the startup error")]
    HostExited { status: ExitStatus },
    #[error(transparent)]
    Core(#[from] gents_desktop_core::client::ClientCoreError),
    #[error(transparent)]
    Paths(#[from] gents_desktop_core::client::PathsError),
    #[cfg(any(unix, windows))]
    #[error(transparent)]
    Listener(#[from] gents_desktop_bridge::host::local::LocalError),
    #[cfg(any(unix, windows))]
    #[error(transparent)]
    EditorStore(#[from] editor_store::EditorStoreError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type Result<T, E = ClientHostError> = std::result::Result<T, E>;

/// Prints the error with its source chain to stderr and exits non-zero.
#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {}", ErrorChain(&error));
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();
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
            if !home.is_absolute() {
                return Err(ClientHostError::HomeNotAbsolute);
            }
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
        Err(ClientHostError::TransportUnavailable)
    }
}
