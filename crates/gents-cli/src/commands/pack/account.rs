//! Registry accounts and releases from the command line: `gents pack login`,
//! `logout`, `whoami`, `info` and `yank`.
//!
//! A token comes from `--token`, then `GENTS_REGISTRY_TOKEN`, then what
//! `gents pack login` saved for that registry in the gents home, so every
//! command that writes to the registry resolves it the same way.

use std::io::BufRead;
use std::path::Path;

use anyhow::{Context, Result};
use gents::pack_registry::credentials;
use serde_json::json;

use super::registry::{resolve_registry_token, resolve_registry_url, RegistryClient};
use crate::cli::{PackAccountArgs, PackInfoArgs, PackLoginArgs, PackOwnerArgs, PackYankArgs};

/// The token a write to `registry` uses.
pub(crate) fn resolve_publish_token(
    explicit: Option<&str>,
    registry: &str,
    home: &Path,
) -> Result<String> {
    if let Some(token) = resolve_registry_token(explicit) {
        return Ok(token);
    }
    credentials::get(home, registry)?.with_context(|| {
        format!("you are not signed in to {registry}; run gents pack login, or pass --token")
    })
}

pub(crate) async fn login(args: PackLoginArgs) -> Result<()> {
    let registry = resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let client = RegistryClient::new(registry.clone());
    let token = match (args.token, args.username) {
        (Some(token), _) => token,
        (None, Some(username)) => {
            anyhow::ensure!(
                args.password_stdin,
                "pass the password on standard input with --password-stdin, or sign in with --token"
            );
            let mut password = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut password)
                .context("reading the password from standard input")?;
            client
                .login(&username, password.trim_end_matches(['\r', '\n']))
                .await?
        }
        (None, None) => anyhow::bail!(
            "pass --username with --password-stdin, or --token with a token from your registry dashboard"
        ),
    };
    let me = client
        .me(&token)
        .await
        .context("the registry did not accept that token")?;
    credentials::set(&home, &registry, &token)?;
    crate::print_json(&json!({ "registry": registry, "signed_in_as": me["username"] }))
}

pub(crate) fn logout(args: PackAccountArgs) -> Result<()> {
    let registry = resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let removed = credentials::remove(&home, &registry)?;
    crate::print_json(&json!({ "registry": registry, "signed_out": removed }))
}

pub(crate) async fn whoami(args: PackAccountArgs) -> Result<()> {
    let registry = resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let token = resolve_publish_token(None, &registry, &home)?;
    let me = RegistryClient::new(registry.clone()).me(&token).await?;
    crate::print_json(&json!({ "registry": registry, "account": me }))
}

pub(crate) async fn info(args: PackInfoArgs) -> Result<()> {
    let (namespace, name) = super::split_namespace(&args.package);
    let client = RegistryClient::new(resolve_registry_url(args.registry.as_deref()));
    crate::print_json(&client.package(namespace, name).await?)
}

pub(crate) async fn yank(args: PackYankArgs) -> Result<()> {
    let (coordinate, version) = args.package.split_once('@').with_context(|| {
        format!(
            "name the version to yank as namespace/name@version, not {:?}",
            args.package
        )
    })?;
    let (namespace, name) = super::split_namespace(coordinate);
    let registry = resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let token = resolve_publish_token(args.token.as_deref(), &registry, &home)?;
    let response = RegistryClient::new(registry)
        .yank(&token, namespace, name, version, args.undo)
        .await?;
    crate::print_json(&response)
}

pub(crate) async fn owner(args: PackOwnerArgs) -> Result<()> {
    let (namespace, name) = super::split_namespace(&args.package);
    let registry = resolve_registry_url(args.registry.as_deref());
    let client = RegistryClient::new(registry.clone());
    let Some(username) = args.transfer else {
        let package = client.package(namespace, name).await?;
        return crate::print_json(&json!({
            "package": format!("{namespace}/{name}"),
            "owner": package["owner"],
        }));
    };
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let token = resolve_publish_token(args.token.as_deref(), &registry, &home)?;
    crate::print_json(
        &client
            .transfer_owner(&token, namespace, name, &username)
            .await?,
    )
}
