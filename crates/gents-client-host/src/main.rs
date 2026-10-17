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
}

#[cfg(unix)]
mod native;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
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
    #[cfg(unix)]
    return if args.serve {
        native::serve(paths).await
    } else {
        native::attach(paths).await
    };
    #[cfg(not(unix))]
    {
        let _ = paths;
        anyhow::bail!("the native client transport is not available on this platform")
    }
}
