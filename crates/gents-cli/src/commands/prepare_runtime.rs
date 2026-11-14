use std::path::Path;

use anyhow::{ensure, Result};
use clap::Parser;

/// A Keychain grant can be scoped to the process receiving it. The explicit
/// editor action keeps that process as the runtime rather than discarding its
/// handles and asking a noninteractive child to reopen them. Automatic
/// preparation never selects this entry point; further prompts are disabled
/// once the existing identity and store key are loaded.
pub async fn prepare_runtime_after_unlock(
    home: &Path,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    ensure!(
        home.is_absolute(),
        "runtime home must be an absolute directory"
    );
    let cli = crate::cli::Cli::try_parse_from([
        std::ffi::OsStr::new("gents"),
        std::ffi::OsStr::new("prepare-runtime"),
        std::ffi::OsStr::new("--home"),
        home.as_os_str(),
    ])?;
    let Some(crate::cli::Command::PrepareRuntime(mut args)) = cli.command else {
        unreachable!("the explicit entry point selects runtime preparation");
    };
    args.serve.keychain_startup_approval = true;
    let telemetry = gents_service::telemetry::init(crate::DEFAULT_LOG_FILTER)?;
    let result = gents_host::prepare_runtime::prepare(
        args,
        runtime,
        std::sync::Arc::new(crate::commands::serve_client::CliServeClient),
    )
    .await;
    telemetry.shutdown();
    result
}
