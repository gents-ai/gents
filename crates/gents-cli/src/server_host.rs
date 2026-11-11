use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use gents_service::args::ToolCeilingArg;

use crate::cli::{Cli, Command};

#[derive(Debug, Clone)]
pub struct ProvisionOptions {
    pub home: PathBuf,
    pub node_name: String,
    pub tool_ceiling: ToolCeilingArg,
    pub tool_root: Option<PathBuf>,
    pub store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice,
    /// Install the Engineer's default packs into a home this call creates.
    pub default_packs: bool,
}

/// Provisioning drives `init`, whose node opening is not `Send`, on a blocking
/// thread of `runtime`; that runtime's threads must have the large stacks the
/// provisioning depth needs.
pub async fn ensure_standard_home(
    runtime: &tokio::runtime::Handle,
    options: ProvisionOptions,
) -> Result<()> {
    gents_store::local::storage_backend::drive_unsendable(runtime, move || {
        ensure_standard_home_inner(options)
    })
    .await
    .context("joining managed Gents provision task")?
}

async fn ensure_standard_home_inner(options: ProvisionOptions) -> Result<()> {
    if let Some(mut stored) = gents_service::home_state::read_init_config(&options.home)? {
        stored.tool_ceiling = options.tool_ceiling;
        stored.tool_root = options
            .tool_root
            .map(|path| path.to_string_lossy().into_owned());
        gents_service::home_state::write_init_config(&options.home, &stored)?;
        return Ok(());
    }

    let tool_package = match options.tool_ceiling {
        ToolCeilingArg::MetaOnly => "minimal",
        ToolCeilingArg::Readonly => "readonly",
        ToolCeilingArg::Readwrite => "yolo",
    };

    let mut argv = vec![
        "gents".to_string(),
        "init".to_string(),
        "--home".to_string(),
        options.home.display().to_string(),
        "--node-name".to_string(),
        options.node_name,
        "--tool-package".to_string(),
        tool_package.to_string(),
        "--engineer".to_string(),
        "--inference-url".to_string(),
        crate::DEFAULT_INIT_ENDPOINT.to_string(),
    ];
    if let Some(tool_root) = options.tool_root {
        argv.push("--tool-root".to_string());
        argv.push(tool_root.display().to_string());
    }
    if options.store_key_custody == gents_store::local::store_key::StoreKeyCustodyChoice::File {
        argv.extend(["--store-key-custody".to_string(), "file".to_string()]);
    }
    let cli = Cli::try_parse_from(argv).context("building standard Gents provision request")?;
    let Some(Command::Init(mut args)) = cli.command else {
        unreachable!("standard provision argv must parse as init")
    };
    // No terminal is owned here, so init must not ask on one: a default pack
    // whose plugins request permissions is left uninstalled and reported.
    args.unattended = true;
    args.no_default_packs = !options.default_packs;
    let initialized = crate::commands::init::init_home(args).await?;
    for pack in initialized["default_packs"]["not_installed"]
        .as_array()
        .into_iter()
        .flatten()
    {
        tracing::warn!(
            pack = pack["pack"].as_str().unwrap_or_default(),
            reason = pack["reason"].as_str().unwrap_or_default(),
            install_with = pack["install_with"].as_str().unwrap_or_default(),
            "an Engineer default pack was not installed"
        );
    }
    Ok(())
}
