//! Embedded Codex TUI pointed at the local Codex shim.
//!
//! Codex publishes its terminal UI as the `codex-tui` crate (pinned in the
//! workspace alongside the other `codex-*` crates). Running it in-process
//! means the user never installs or configures Codex: we construct the TUI's
//! CLI state directly and hand it the shim's WebSocket endpoint. Approvals
//! and sandboxing are bypassed on the Codex side because the Gents runtime
//! owns tool gating — the tool preset chosen at `init` is the real
//! permission boundary.

#[cfg(feature = "codex-tui")]
use std::{io::IsTerminal, time::Duration};

#[cfg(feature = "codex-tui")]
use anyhow::{anyhow, Context};
use anyhow::{bail, Result};
#[cfg(feature = "codex-tui")]
use clap::Parser;
#[cfg(feature = "codex-tui")]
use codex_arg0::Arg0DispatchPaths;
#[cfg(feature = "codex-tui")]
use codex_tui::{Cli as CodexTuiCli, ExitReason, RemoteAppServerEndpoint};
#[cfg(feature = "codex-tui")]
use tokio::net::TcpStream;

use crate::cli::args::CodexArgs;

#[cfg(feature = "codex-tui")]
const SHIM_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(feature = "codex-tui")]
pub(crate) async fn codex(args: CodexArgs) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("`gents codex` is interactive and needs a terminal; use `gents chat` for scripted turns");
    }

    let endpoint = with_remote_auth_token(
        resolve_endpoint(&args.remote)?,
        read_remote_auth_token(args.remote_auth_token_env.as_deref())?,
    )?;
    probe_shim(&endpoint).await?;

    let cli = build_tui_cli(&args);
    let arg0_paths = Arg0DispatchPaths {
        codex_self_exe: std::env::current_exe().ok(),
        ..Default::default()
    };

    let exit_info = codex_tui::run_main(
        cli,
        arg0_paths,
        codex_config::LoaderOverrides::default(),
        Some(endpoint),
    )
    .await
    .context("running the embedded Codex TUI")?;

    match exit_info.exit_reason {
        ExitReason::Fatal(message) => bail!("codex exited: {message}"),
        ExitReason::UserRequested => Ok(()),
    }
}

#[cfg(not(feature = "codex-tui"))]
pub(crate) async fn codex(_args: CodexArgs) -> Result<()> {
    bail!(
        "`gents codex` was built without the `codex-tui` feature; rebuild with default features or connect an external Codex client to the running shim"
    )
}

#[cfg(feature = "codex-tui")]
fn resolve_endpoint(remote: &str) -> Result<RemoteAppServerEndpoint> {
    codex_tui::resolve_remote_addr(remote)
        .map_err(|error| anyhow!("invalid --remote address {remote:?}: {error}"))
}

#[cfg(feature = "codex-tui")]
fn read_remote_auth_token(env_name: Option<&str>) -> Result<Option<String>> {
    env_name
        .map(|name| read_remote_auth_token_with(name, |name| std::env::var(name)))
        .transpose()
}

#[cfg(feature = "codex-tui")]
fn read_remote_auth_token_with<F>(env_name: &str, read: F) -> Result<String>
where
    F: FnOnce(&str) -> std::result::Result<String, std::env::VarError>,
{
    let token = read(env_name)
        .with_context(|| format!("reading remote app-server token from {env_name}"))?;
    let token = token.trim();
    if token.is_empty() {
        bail!("remote app-server token in {env_name} is empty");
    }
    Ok(token.to_string())
}

#[cfg(feature = "codex-tui")]
fn with_remote_auth_token(
    mut endpoint: RemoteAppServerEndpoint,
    auth_token: Option<String>,
) -> Result<RemoteAppServerEndpoint> {
    let Some(auth_token) = auth_token else {
        return Ok(endpoint);
    };
    if !codex_tui::remote_addr_supports_auth_token(&endpoint) {
        bail!("remote app-server tokens require a wss:// or loopback ws:// endpoint");
    }
    let RemoteAppServerEndpoint::WebSocket {
        auth_token: slot, ..
    } = &mut endpoint
    else {
        bail!("remote app-server tokens are only supported for WebSocket endpoints");
    };
    *slot = Some(auth_token);
    Ok(endpoint)
}

#[cfg(feature = "codex-tui")]
fn build_tui_cli(args: &CodexArgs) -> CodexTuiCli {
    let mut cli = CodexTuiCli::parse_from(["codex"]);
    cli.dangerously_bypass_approvals_and_sandbox = true;
    cli.no_alt_screen = args.no_alt_screen;
    cli.prompt = args.prompt.clone();
    cli.config_overrides
        .raw_overrides
        .push("show_raw_agent_reasoning=true".to_string());
    cli
}

#[cfg(feature = "codex-tui")]
async fn probe_shim(endpoint: &RemoteAppServerEndpoint) -> Result<()> {
    let Some(authority) = probe_authority(endpoint) else {
        return Ok(());
    };
    let connect = TcpStream::connect(&authority);
    match tokio::time::timeout(SHIM_PROBE_TIMEOUT, connect).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            bail!("no Codex shim listening at {authority} ({error}); start `gents server` first")
        }
        Err(_) => {
            bail!("timed out reaching the Codex shim at {authority}; start `gents server` first")
        }
    }
}

#[cfg(feature = "codex-tui")]
fn probe_authority(endpoint: &RemoteAppServerEndpoint) -> Option<String> {
    let RemoteAppServerEndpoint::WebSocket { websocket_url, .. } = endpoint else {
        return None;
    };
    let authority = websocket_url
        .strip_prefix("ws://")
        .or_else(|| websocket_url.strip_prefix("wss://"))?
        .split(['/', '?', '#'])
        .next()?;
    let (_, port) = authority.rsplit_once(':')?;
    if port.is_empty() || port.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    Some(authority.to_string())
}

#[cfg(all(test, feature = "codex-tui"))]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn args(remote: &str) -> CodexArgs {
        CodexArgs {
            remote: remote.to_string(),
            remote_auth_token_env: None,
            no_alt_screen: false,
            prompt: None,
        }
    }

    #[test]
    fn default_remote_resolves_to_loopback_websocket() {
        let endpoint = resolve_endpoint(crate::DEFAULT_CODEX_REMOTE).expect("resolves");
        let RemoteAppServerEndpoint::WebSocket {
            websocket_url,
            auth_token,
        } = endpoint
        else {
            panic!("expected websocket endpoint");
        };
        assert!(
            websocket_url.starts_with("ws://127.0.0.1:9292"),
            "{websocket_url}"
        );
        assert_eq!(auth_token, None);
    }

    #[test]
    fn invalid_remote_is_rejected() {
        assert!(resolve_endpoint("not-a-url").is_err());
    }

    #[test]
    fn remote_auth_token_is_attached_to_loopback_websocket() {
        let endpoint = with_remote_auth_token(
            resolve_endpoint(crate::DEFAULT_CODEX_REMOTE).expect("resolves"),
            Some("secret".to_string()),
        )
        .expect("token accepted");
        let RemoteAppServerEndpoint::WebSocket { auth_token, .. } = endpoint else {
            panic!("expected websocket endpoint");
        };
        assert_eq!(auth_token.as_deref(), Some("secret"));
    }

    #[test]
    fn remote_auth_token_rejects_plaintext_remote_websocket() {
        let endpoint = resolve_endpoint("ws://192.0.2.10:9292/").expect("resolves");
        assert!(with_remote_auth_token(endpoint, Some("secret".to_string())).is_err());
    }

    #[test]
    fn remote_auth_token_env_is_trimmed_and_required() {
        assert_eq!(
            read_remote_auth_token_with("GENTS_TOKEN", |_| Ok("  secret  ".to_string()))
                .expect("token"),
            "secret"
        );
        assert!(read_remote_auth_token_with("GENTS_TOKEN", |_| Ok("  ".to_string())).is_err());
        assert!(read_remote_auth_token_with("GENTS_TOKEN", |_| {
            Err(std::env::VarError::NotPresent)
        })
        .is_err());
    }

    #[test]
    fn remote_auth_token_env_flag_parses() {
        let cli = Cli::try_parse_from([
            "gents",
            "codex",
            "--remote-auth-token-env",
            "GENTS_REMOTE_TOKEN",
        ])
        .expect("parse");
        let Command::Codex(args) = cli.command else {
            panic!("expected codex command");
        };
        assert_eq!(
            args.remote_auth_token_env.as_deref(),
            Some("GENTS_REMOTE_TOKEN")
        );
        assert!(
            Cli::try_parse_from(["gents", "codex", "--remote-auth-token-env", "unsafe;name",])
                .is_err()
        );
    }

    #[test]
    fn tui_cli_bypasses_codex_side_gating_and_carries_prompt() {
        let mut args = args(crate::DEFAULT_CODEX_REMOTE);
        args.no_alt_screen = true;
        args.prompt = Some("hello".to_string());
        let cli = build_tui_cli(&args);
        assert!(cli.dangerously_bypass_approvals_and_sandbox);
        assert!(cli.no_alt_screen);
        assert_eq!(cli.prompt.as_deref(), Some("hello"));
        assert!(cli
            .config_overrides
            .raw_overrides
            .iter()
            .any(|value| value == "show_raw_agent_reasoning=true"));
    }

    #[test]
    fn probe_authority_extracts_host_port() {
        let endpoint = resolve_endpoint("ws://127.0.0.1:9292/").expect("resolves");
        assert_eq!(
            probe_authority(&endpoint).as_deref(),
            Some("127.0.0.1:9292")
        );
    }

    #[test]
    fn probe_skips_unix_sockets() {
        let endpoint = resolve_endpoint("unix:///tmp/shim.sock").expect("resolves");
        assert_eq!(probe_authority(&endpoint), None);
    }
}
