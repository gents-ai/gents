use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::net::TcpStream;
use url::{Host, Url};

use crate::cli::args::CodexArgs;
use crate::client_settings::InterfaceClient;
use crate::commands::codex_shim::RuntimeIdentity;
use crate::commands::start::interface_install_message;

const SHIM_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const CODEX_BIN_ENV: &str = "GENTS_CODEX_BIN";

#[derive(Clone, Debug, PartialEq, Eq)]
enum RemoteEndpoint {
    WebSocket {
        authority: String,
        secure: bool,
        loopback: bool,
    },
    UnixSocket,
}

pub(crate) async fn codex(args: CodexArgs) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("`gents codex` is interactive and needs a terminal; use `gents chat` for scripted turns");
    }
    open_codex(args).await
}

pub(crate) async fn open_codex(args: CodexArgs) -> Result<()> {
    let endpoint = resolve_endpoint(&args.remote)?;
    validate_remote_auth(&endpoint, args.remote_auth_token_env.as_deref())?;
    probe_shim(&endpoint).await?;
    launch_interface(
        "Codex",
        &interface_bin(CODEX_BIN_ENV, "codex"),
        &codex_command_args(&args),
        &interface_install_message(InterfaceClient::Codex),
    )
    .await
}

pub(crate) fn interface_bin(env_name: &str, default: &str) -> String {
    std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub(crate) async fn launch_interface(
    name: &str,
    bin: &str,
    args: &[String],
    install_hint: &str,
) -> Result<()> {
    let status = tokio::process::Command::new(bin)
        .args(args)
        .status()
        .await
        .with_context(|| format!("launching `{bin}`: {install_hint}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{name} exited with {status}")
    }
}

/// Anything but a decoded success is `None`: nothing here can tell an older
/// `gents` from another program, and no caller treats them differently.
pub(crate) async fn read_runtime_identity(
    authority: &str,
    bearer: Option<&str>,
) -> Option<RuntimeIdentity> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(SHIM_PROBE_TIMEOUT)
        .build()
        .ok()?;
    let mut request = client.get(format!("http://{authority}/runtime"));
    if let Some(bearer) = bearer {
        request = request.bearer_auth(bearer);
    }
    request
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

/// The endpoint is a fixed port, so answering it proves nothing about whose
/// runtime this is. Only a matching DID passes.
pub(crate) async fn ensure_endpoint_is_this_homes(remote: &str, expected_did: &str) -> Result<()> {
    let identity = match resolve_endpoint(remote)? {
        RemoteEndpoint::WebSocket { authority, .. } => {
            read_runtime_identity(&authority, None).await
        }
        RemoteEndpoint::UnixSocket => None,
    };
    if identity
        .as_ref()
        .is_some_and(|identity| identity.node_did == expected_did)
    {
        return Ok(());
    }
    let other_home = identity.map_or(String::new(), |identity| {
        let home = identity.home.display();
        format!(" It serves Gents home {home}; run `gents --home {home}` to open it.")
    });
    bail!(
        "the Codex endpoint {remote} does not belong to this Gents home.{other_home} To use this home, stop whatever holds that endpoint."
    )
}

pub(crate) async fn endpoint_answers(remote: &str) -> bool {
    match resolve_endpoint(remote) {
        Ok(endpoint) => probe_shim(&endpoint).await.is_ok(),
        Err(_) => false,
    }
}

fn codex_command_args(args: &CodexArgs) -> Vec<String> {
    let mut command = vec![
        "--remote".to_string(),
        args.remote.clone(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
        "--config".to_string(),
        "show_raw_agent_reasoning=true".to_string(),
    ];
    if let Some(env_name) = &args.remote_auth_token_env {
        command.push("--remote-auth-token-env".to_string());
        command.push(env_name.clone());
    }
    if args.no_alt_screen {
        command.push("--no-alt-screen".to_string());
    }
    if let Some(prompt) = &args.prompt {
        command.push(prompt.clone());
    }
    command
}

fn resolve_endpoint(remote: &str) -> Result<RemoteEndpoint> {
    if remote.starts_with("unix://") {
        return Ok(RemoteEndpoint::UnixSocket);
    }

    let url = Url::parse(remote).with_context(|| format!("invalid --remote address {remote:?}"))?;
    let secure = match url.scheme() {
        "ws" => false,
        "wss" => true,
        scheme => bail!("invalid --remote scheme {scheme:?}; expected ws, wss, or unix"),
    };
    let host = url
        .host()
        .context("Codex shim WebSocket endpoint is missing a host")?;
    let port = url
        .port_or_known_default()
        .context("Codex shim WebSocket endpoint is missing a port")?;
    let loopback = match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    };
    let authority = match host {
        Host::Ipv6(address) => format!("[{address}]:{port}"),
        _ => format!("{}:{port}", url.host_str().expect("host checked above")),
    };
    Ok(RemoteEndpoint::WebSocket {
        authority,
        secure,
        loopback,
    })
}

fn validate_remote_auth(endpoint: &RemoteEndpoint, auth_env: Option<&str>) -> Result<()> {
    let Some(auth_env) = auth_env else {
        return Ok(());
    };
    let supported = matches!(
        endpoint,
        RemoteEndpoint::WebSocket { secure: true, .. }
            | RemoteEndpoint::WebSocket { loopback: true, .. }
    );
    if !supported {
        bail!(
            "remote app-server token from {auth_env} requires a wss:// or loopback ws:// endpoint"
        );
    }
    Ok(())
}

async fn probe_shim(endpoint: &RemoteEndpoint) -> Result<()> {
    let RemoteEndpoint::WebSocket { authority, .. } = endpoint else {
        return Ok(());
    };
    let connect = TcpStream::connect(authority);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command as GentsCommand};
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
        assert_eq!(
            resolve_endpoint(crate::DEFAULT_CODEX_REMOTE).expect("resolves"),
            RemoteEndpoint::WebSocket {
                authority: "127.0.0.1:9292".to_string(),
                secure: false,
                loopback: true,
            }
        );
    }

    #[test]
    fn invalid_remote_is_rejected() {
        assert!(resolve_endpoint("not-a-url").is_err());
        assert!(resolve_endpoint("http://127.0.0.1:9292").is_err());
    }

    #[test]
    fn remote_auth_token_requires_secure_or_loopback_websocket() {
        let loopback = resolve_endpoint("ws://127.0.0.1:9292/").expect("resolves");
        assert!(validate_remote_auth(&loopback, Some("GENTS_TOKEN")).is_ok());

        let secure = resolve_endpoint("wss://example.com/").expect("resolves");
        assert!(validate_remote_auth(&secure, Some("GENTS_TOKEN")).is_ok());

        let plaintext = resolve_endpoint("ws://192.0.2.10:9292/").expect("resolves");
        assert!(validate_remote_auth(&plaintext, Some("GENTS_TOKEN")).is_err());

        let unix = resolve_endpoint("unix:///tmp/shim.sock").expect("resolves");
        assert!(validate_remote_auth(&unix, Some("GENTS_TOKEN")).is_err());
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
        let Some(GentsCommand::Codex(args)) = cli.command else {
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
    fn external_codex_args_preserve_gents_tui_contract() {
        let mut args = args(crate::DEFAULT_CODEX_REMOTE);
        args.remote_auth_token_env = Some("GENTS_TOKEN".to_string());
        args.no_alt_screen = true;
        args.prompt = Some("hello".to_string());

        assert_eq!(
            codex_command_args(&args),
            vec![
                "--remote",
                crate::DEFAULT_CODEX_REMOTE,
                "--dangerously-bypass-approvals-and-sandbox",
                "--config",
                "show_raw_agent_reasoning=true",
                "--remote-auth-token-env",
                "GENTS_TOKEN",
                "--no-alt-screen",
                "hello",
            ]
        );
    }

    #[test]
    fn probe_skips_unix_sockets() {
        assert_eq!(
            resolve_endpoint("unix:///tmp/shim.sock").expect("resolves"),
            RemoteEndpoint::UnixSocket
        );
        assert_eq!(
            resolve_endpoint("unix://shim.sock").expect("resolves"),
            RemoteEndpoint::UnixSocket
        );
    }
}
