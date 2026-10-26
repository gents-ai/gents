use std::path::Path;

use anyhow::{Context, Result, ensure};
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
    let crate::cli::Command::PrepareRuntime(mut args) = cli.command else {
        unreachable!("the explicit entry point selects runtime preparation");
    };
    args.keychain_startup_approval = true;
    let telemetry = crate::telemetry::init(crate::DEFAULT_LOG_FILTER)?;
    let result = super::prepare(args, runtime).await;
    telemetry.shutdown();
    result
}

/// Only an explicit user unlock action may call this entry point. It must run
/// in the same executable as the managed server so macOS custody approval binds
/// to that helper. Loading existing keys neither opens the database nor creates
/// replacement keys, changes custody, or exports secret bytes to the editor.
/// OS custody may gate private-key use separately from loading its handle, so
/// unlock exercises signing without retaining or transmitting the signature.
pub async fn unlock_runtime_keys(home: &Path) -> Result<()> {
    ensure!(
        home.is_absolute(),
        "runtime home must be an absolute directory"
    );
    let home = std::fs::canonicalize(home)
        .with_context(|| format!("opening initialized runtime home {}", home.display()))?;
    let (config, _) = super::read_home(&home)?;
    if let Some(encryption) = config.store_encryption.as_ref() {
        drop(encryption.load(
            &gents::store_key::home_key_file(&home),
            &gents::home::default_data_dir(&home),
        )?);
    }
    let identity = crate::home_state::load_initialized_home_identity(&home, &config)?;
    drop(identity.sign(b"gents.runtime-key-unlock.v1").await?);
    Ok(())
}
