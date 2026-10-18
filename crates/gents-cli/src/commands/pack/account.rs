//! Registry accounts and releases from the command line: `gents pack login`,
//! `logout`, `whoami`, `info` and `yank`.
//!
//! A token comes from `--token`, then `GENTS_REGISTRY_TOKEN`, then what
//! `gents pack login` saved for that registry in the gents home, so every
//! command that writes to the registry resolves it the same way.

use std::path::Path;

use anyhow::{Context, Result};
use gents::pack_registry::credentials;
use serde_json::json;

use super::registry::{
    read_stdin_line, resolve_registry_token, resolve_registry_url, resolve_token_flag,
    RegistryClient,
};
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

/// Exchanges `username`/`password` for a token, refusing an empty password
/// (after `--password-stdin` trims its trailing newline) rather than sending
/// it to the registry.
async fn login_with_password(
    client: &RegistryClient,
    username: &str,
    password: &str,
) -> Result<String> {
    anyhow::ensure!(
        !password.is_empty(),
        "the password read from standard input is empty"
    );
    client.login(username, password).await
}

pub(crate) async fn login(args: PackLoginArgs) -> Result<()> {
    let registry = resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let client = RegistryClient::new(registry.clone());
    let token_flag = resolve_token_flag(args.token, args.token_stdin)?;
    let token = match (token_flag, args.username) {
        (Some(token), _) => token,
        (None, Some(username)) => {
            anyhow::ensure!(
                args.password_stdin,
                "pass the password on standard input with --password-stdin, or sign in with --token"
            );
            let password = read_stdin_line("password")?;
            login_with_password(&client, &username, &password).await?
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
    let token_flag = resolve_token_flag(args.token, args.token_stdin)?;
    let token = resolve_publish_token(token_flag.as_deref(), &registry, &home)?;
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
    anyhow::ensure!(
        args.yes,
        "transferring {namespace}/{name} to {username} cannot be undone; pass --yes to confirm"
    );
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let token_flag = resolve_token_flag(args.token, args.token_stdin)?;
    let token = resolve_publish_token(token_flag.as_deref(), &registry, &home)?;
    crate::print_json(
        &client
            .transfer_owner(&token, namespace, name, &username)
            .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PackAccountArgs;
    use crate::commands::pack::registry::tests::{
        serve_fake_pack, FAKE_PASSWORD, FAKE_TOKEN, FAKE_USERNAME,
    };

    #[tokio::test]
    async fn an_empty_password_is_refused_before_it_reaches_the_registry() {
        let client = RegistryClient::new("http://127.0.0.1:1".to_string());
        let error = login_with_password(&client, "demo", "").await.unwrap_err();
        assert!(
            format!("{error:#}").contains("password") && format!("{error:#}").contains("empty"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn login_exchanges_a_password_for_a_token_the_fake_registry_issued() {
        let (base_url, _state) =
            serve_fake_pack("plain_pack", "1.0.0", Vec::new(), String::new()).await;
        let client = RegistryClient::new(base_url);
        let token = login_with_password(&client, FAKE_USERNAME, FAKE_PASSWORD)
            .await
            .unwrap();
        assert_eq!(token, FAKE_TOKEN);
        let wrong = login_with_password(&client, FAKE_USERNAME, "not-the-password").await;
        assert!(wrong.is_err());
    }

    #[tokio::test]
    async fn login_whoami_and_logout_round_trip_against_the_fake_registry() {
        let (base_url, _state) =
            serve_fake_pack("plain_pack", "1.0.0", Vec::new(), String::new()).await;
        let home = tempfile::tempdir().unwrap();

        login(PackLoginArgs {
            username: None,
            password_stdin: false,
            token: Some(FAKE_TOKEN.to_owned()),
            token_stdin: false,
            registry: Some(base_url.clone()),
            home: Some(home.path().to_path_buf()),
        })
        .await
        .unwrap();
        assert_eq!(
            credentials::get(home.path(), &base_url).unwrap().as_deref(),
            Some(FAKE_TOKEN)
        );

        let report = crate::request_helpers::capture_report(whoami(PackAccountArgs {
            registry: Some(base_url.clone()),
            home: Some(home.path().to_path_buf()),
        }))
        .await
        .unwrap();
        assert_eq!(report["account"]["username"], FAKE_USERNAME);

        assert!(logout(PackAccountArgs {
            registry: Some(base_url.clone()),
            home: Some(home.path().to_path_buf()),
        })
        .is_ok());
        assert_eq!(credentials::get(home.path(), &base_url).unwrap(), None);
    }

    #[tokio::test]
    async fn yank_and_undo_reach_the_fake_registry_with_the_bearer_token() {
        let (base_url, state) =
            serve_fake_pack("plain_pack", "1.0.0", Vec::new(), String::new()).await;

        yank(PackYankArgs {
            package: "gents/plain_pack@1.0.0".to_owned(),
            undo: false,
            registry: Some(base_url.clone()),
            token: Some(FAKE_TOKEN.to_owned()),
            token_stdin: false,
            home: None,
        })
        .await
        .unwrap();
        let (ns, name, version, undo, token) = state.last_yank.lock().unwrap().clone().unwrap();
        assert_eq!(
            (ns.as_str(), name.as_str(), version.as_str(), undo),
            ("gents", "plain_pack", "1.0.0", false)
        );
        assert_eq!(token, FAKE_TOKEN);

        yank(PackYankArgs {
            package: "gents/plain_pack@1.0.0".to_owned(),
            undo: true,
            registry: Some(base_url),
            token: Some(FAKE_TOKEN.to_owned()),
            token_stdin: false,
            home: None,
        })
        .await
        .unwrap();
        assert!(state.last_yank.lock().unwrap().as_ref().unwrap().3);
    }

    #[tokio::test]
    async fn an_owner_transfer_without_yes_is_refused_naming_the_package_and_recipient() {
        let (base_url, state) =
            serve_fake_pack("plain_pack", "1.0.0", Vec::new(), String::new()).await;
        let error = owner(PackOwnerArgs {
            package: "acme/widget".to_owned(),
            transfer: Some("new_owner".to_owned()),
            yes: false,
            registry: Some(base_url),
            token: Some(FAKE_TOKEN.to_owned()),
            token_stdin: false,
            home: None,
        })
        .await
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("acme/widget"), "{message}");
        assert!(message.contains("new_owner"), "{message}");
        assert!(message.contains("cannot be undone"), "{message}");
        assert!(state.last_owner_transfer.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn an_owner_transfer_with_yes_reaches_the_fake_registry_with_the_bearer_token() {
        let (base_url, state) =
            serve_fake_pack("plain_pack", "1.0.0", Vec::new(), String::new()).await;
        owner(PackOwnerArgs {
            package: "acme/widget".to_owned(),
            transfer: Some("new_owner".to_owned()),
            yes: true,
            registry: Some(base_url),
            token: Some(FAKE_TOKEN.to_owned()),
            token_stdin: false,
            home: None,
        })
        .await
        .unwrap();
        let (ns, name, new_owner, token) =
            state.last_owner_transfer.lock().unwrap().clone().unwrap();
        assert_eq!(
            (ns.as_str(), name.as_str(), new_owner.as_str()),
            ("acme", "widget", "new_owner")
        );
        assert_eq!(token, FAKE_TOKEN);
    }

    #[test]
    fn resolve_token_flag_passes_through_an_explicit_token_without_touching_stdin() {
        assert_eq!(
            resolve_token_flag(Some("explicit".to_owned()), false)
                .unwrap()
                .as_deref(),
            Some("explicit")
        );
        assert_eq!(resolve_token_flag(None, false).unwrap(), None);
        // The `--token-stdin` branch reads real standard input (see
        // `read_stdin_line`); it is exercised by the CLI, not a unit test,
        // since process stdin cannot be redirected per-test here.
    }
}
