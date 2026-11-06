use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use gents::codex_shim_binding::admits_connection;
use relay::AgentRelay;
use tokio::net::TcpStream;
use url::{Host, Url};

use crate::cli::args::CodexArgs;
use crate::client_settings::InterfaceClient;
use crate::commands::codex_shim::RuntimeIdentity;
use crate::commands::start::interface_install_message;

mod relay;

const SHIM_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const PUBLISH_WAIT: Duration = Duration::from_secs(5);
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
        bail!("`gents codex` is interactive and needs a terminal; use `gents -p MESSAGE` for scripted turns");
    }
    open_codex(args).await
}

pub(crate) async fn open_codex(mut args: CodexArgs) -> Result<()> {
    let endpoint = resolve_endpoint(&args.remote)?;
    let upstream = args
        .agent
        .as_ref()
        .map(|_| agent_relay_upstream(&endpoint))
        .transpose()?;
    validate_remote_auth(&endpoint, args.remote_auth_token_env.as_deref())?;
    probe_shim(&endpoint).await?;
    let bearer = args
        .remote_auth_token_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok());
    let _relay_while_codex_runs = match (upstream, &args.agent) {
        (Some(upstream), Some(agent)) => {
            preflight_agent(&upstream, bearer.as_deref(), agent, PUBLISH_WAIT).await?;
            let relay = AgentRelay::bind(upstream, agent).await?;
            args.remote = relay.remote();
            Some(relay)
        }
        _ => None,
    };
    let (cd, outside_notice) = terminal_placement(&endpoint, bearer.as_deref())
        .await
        .unwrap_or_default();
    if let Some(notice) = &outside_notice {
        eprintln!("{notice}");
    }
    // Codex takes the screen right after the first notice, which hides it until Codex exits.
    launch_interface(
        "Codex",
        &interface_bin(CODEX_BIN_ENV, "codex"),
        &codex_command_args(&args, cd.as_deref()),
        &interface_install_message(InterfaceClient::Codex),
        outside_notice.as_deref(),
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
    notice_after_exit: Option<&str>,
) -> Result<()> {
    let status = tokio::process::Command::new(bin)
        .args(args)
        .status()
        .await
        .with_context(|| format!("launching `{bin}`: {install_hint}"))?;
    if let Some(notice) = notice_after_exit {
        eprintln!("{notice}");
    }
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

/// `None` for anything that cannot say, as `read_runtime_identity` does.
pub(crate) async fn runtime_identity_at(remote: &str) -> Option<RuntimeIdentity> {
    match resolve_endpoint(remote).ok()? {
        RemoteEndpoint::WebSocket { authority, .. } => {
            read_runtime_identity(&authority, None).await
        }
        RemoteEndpoint::UnixSocket => None,
    }
}

/// The endpoint is a fixed port, so answering it proves nothing about whose
/// runtime this is. Only a matching DID passes.
pub(crate) async fn ensure_endpoint_is_this_homes(remote: &str, expected_did: &str) -> Result<()> {
    let identity = runtime_identity_at(remote).await;
    if identity
        .as_ref()
        .is_some_and(|identity| identity.node_did == expected_did)
    {
        return Ok(());
    }
    bail!("{}", foreign_endpoint_refusal(remote, identity.as_ref()))
}

fn foreign_endpoint_refusal(remote: &str, identity: Option<&RuntimeIdentity>) -> String {
    let other_home = identity.map_or(String::new(), |identity| {
        format!(
            " It serves Gents home {}; run `gents --home {}` to open it.",
            identity.home.display(),
            crate::shell_word(&identity.home)
        )
    });
    format!(
        "could not confirm the Codex endpoint {remote} belongs to this Gents home.{other_home} To use this home, stop whatever holds that endpoint."
    )
}

pub(crate) async fn endpoint_answers(remote: &str) -> bool {
    match resolve_endpoint(remote) {
        Ok(endpoint) => probe_shim(&endpoint).await.is_ok(),
        Err(_) => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PreflightDecision {
    Go,
    WaitForPublish,
    Refuse(String),
}

fn preflight_decision(identity: &RuntimeIdentity, agent: &str) -> PreflightDecision {
    let Some(runnable) = &identity.runnable_agents else {
        return PreflightDecision::Refuse(
            "`--agent` needs a local runtime with no pinned agent, and this one runs with `--codex-shim-agent-id` or `--codex-shim-public-url`."
                .to_string(),
        );
    };
    if runnable.is_empty() {
        return PreflightDecision::WaitForPublish;
    }
    if admits_connection(agent, runnable.iter().map(String::as_str)) {
        return PreflightDecision::Go;
    }
    PreflightDecision::Refuse(format!(
        "unknown agent {agent:?}: this runtime cannot run it. See `gents config agent list`."
    ))
}

/// The set can be empty when `/runtime` answers, and no cross-process notification of a
/// publish exists, so `/runtime` is read again until `published` holds or the wait ends.
/// Returns the last read, or `None` once a read fails.
async fn read_identity_until<F>(
    authority: &str,
    bearer: Option<&str>,
    publish_wait: Duration,
    published: F,
) -> Option<RuntimeIdentity>
where
    F: Fn(&RuntimeIdentity) -> bool,
{
    let deadline = Instant::now() + publish_wait;
    loop {
        let identity = read_runtime_identity(authority, bearer).await?;
        if published(&identity) || Instant::now() >= deadline {
            return Some(identity);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Only an empty set is looked at again. A home whose backend is down stays empty, and this
/// gives up rather than call the agent unknown.
async fn preflight_agent(
    authority: &str,
    bearer: Option<&str>,
    agent: &str,
    publish_wait: Duration,
) -> Result<()> {
    let identity = read_identity_until(authority, bearer, publish_wait, |identity| {
        preflight_decision(identity, agent) != PreflightDecision::WaitForPublish
    })
    .await;
    let Some(identity) = identity else {
        bail!("{}", foreign_endpoint_refusal(authority, None));
    };
    match preflight_decision(&identity, agent) {
        PreflightDecision::Go => Ok(()),
        PreflightDecision::Refuse(message) => bail!("{message}"),
        PreflightDecision::WaitForPublish => bail!(
            "this runtime has published no runnable agent, so {agent:?} cannot open yet. Check `gents status`."
        ),
    }
}

/// Waits until the runtime at `remote` publishes `agent`, so a just-made agent is not refused
/// as unknown by the preflight that opening it runs. The preflight still decides.
pub(crate) async fn wait_for_agent_to_publish(remote: &str, agent: &str, publish_wait: Duration) {
    let Ok(RemoteEndpoint::WebSocket { authority, .. }) = resolve_endpoint(remote) else {
        return;
    };
    read_identity_until(&authority, None, publish_wait, |identity| {
        identity
            .runnable_agents
            .as_ref()
            .is_none_or(|runnable| admits_connection(agent, runnable.iter().map(String::as_str)))
    })
    .await;
}

/// Plain `gents` opens the bound agent, so that one has to be runnable. An agent named with
/// `--agent` gets its own check when it opens, so here only an empty set stops it.
fn can_open(identity: &RuntimeIdentity, agent_given: bool) -> bool {
    identity.runnable_agents.as_ref().is_none_or(|runnable| {
        if agent_given {
            !runnable.is_empty()
        } else {
            admits_connection(&identity.bound_agent, runnable.iter().map(String::as_str))
        }
    })
}

fn cannot_open_refusal(identity: &RuntimeIdentity, home: &Path) -> String {
    let home = crate::shell_word(home);
    let what = if identity.runnable_agents.as_ref().is_some_and(Vec::is_empty) {
        "no agent can run yet on this home's runtime".to_string()
    } else {
        format!(
            "this home's default agent {:?} cannot run yet, though `gents --agent <id>` can open another (see `gents config agent list`)",
            identity.bound_agent
        )
    };
    format!(
        "{what}. Check `gents status --home {home}`. If its model cannot be reached, start the model, or change its address with `gents config backend set --file <backend.json> --home {home}` (`gents config backend list --home {home}` names the backend to `show`)."
    )
}

/// With nothing runnable an interface only fails in its own words, as Codex does with
/// `model/list returned no models for TUI bootstrap`. A pinned runtime publishes no set,
/// and a runtime that cannot say is left for the interface to report.
pub(crate) async fn ensure_an_agent_can_run(
    remote: &str,
    home: &Path,
    agent_given: bool,
    publish_wait: Duration,
) -> Result<()> {
    let Ok(RemoteEndpoint::WebSocket { authority, .. }) = resolve_endpoint(remote) else {
        return Ok(());
    };
    let identity = read_identity_until(&authority, None, publish_wait, |identity| {
        can_open(identity, agent_given)
    })
    .await;
    let Some(identity) = identity else {
        return Ok(());
    };
    ensure!(
        can_open(&identity, agent_given),
        "{}",
        cannot_open_refusal(&identity, home)
    );
    Ok(())
}

/// The route exists only on a local runtime, so nothing else can serve `--agent`.
fn agent_relay_upstream(endpoint: &RemoteEndpoint) -> Result<String> {
    match endpoint {
        RemoteEndpoint::WebSocket {
            authority,
            secure: false,
            loopback: true,
        } => Ok(authority.clone()),
        _ => bail!("`--agent` needs a loopback ws:// endpoint, because only a local runtime serves one route per agent"),
    }
}

/// A directory on this machine means nothing to a runtime on another, so only a
/// loopback endpoint gets one, and only one that says where its threads may go.
async fn terminal_placement(
    endpoint: &RemoteEndpoint,
    bearer: Option<&str>,
) -> Option<(Option<PathBuf>, Option<String>)> {
    let RemoteEndpoint::WebSocket {
        authority,
        loopback: true,
        ..
    } = endpoint
    else {
        return None;
    };
    let mut identity = read_runtime_identity(authority, bearer).await?;
    identity.tool_root = canonical_tool_root(identity.tool_root).await;
    let cwd = std::env::current_dir().ok()?;
    Some((
        terminal_cd_arg(&cwd, identity.tool_root.as_deref()),
        outside_tool_root_notice(&cwd, &identity),
    ))
}

/// `current_dir` is already physical, so a root recorded through a symlink would read a
/// terminal really inside it as outside. A root that no longer resolves is compared as given.
async fn canonical_tool_root(root: Option<PathBuf>) -> Option<PathBuf> {
    let root = root?;
    Some(tokio::fs::canonicalize(&root).await.unwrap_or(root))
}

/// Admission refuses a placement outside the tool root, so a directory outside it
/// stays unset and the thread takes the runtime's default.
fn terminal_cd_arg(cwd: &Path, tool_root: Option<&Path>) -> Option<PathBuf> {
    tool_root
        .filter(|root| cwd.starts_with(root))
        .map(|_| cwd.to_path_buf())
}

fn outside_tool_root_notice(cwd: &Path, identity: &RuntimeIdentity) -> Option<String> {
    let root = identity.tool_root.as_deref()?;
    terminal_cd_arg(cwd, Some(root)).is_none().then(|| {
        format!(
            "{} is outside this home's tool root, so the thread will use {}",
            cwd.display(),
            identity.default_thread_cwd.display()
        )
    })
}

fn codex_command_args(args: &CodexArgs, cd: Option<&Path>) -> Vec<String> {
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
    if let Some(cd) = cd {
        command.push("--cd".to_string());
        command.push(cd.display().to_string());
    }
    // Codex's prompt is a positional beside its subcommands, so without the separator a
    // prompt that is a subcommand's name, such as `login`, would run that subcommand.
    if let Some(prompt) = &args.prompt {
        command.extend(["--".to_string(), prompt.clone()]);
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
    let loopback = is_loopback_host(&host);
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

pub(crate) fn is_loopback_host(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
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
            bail!("no Codex shim listening at {authority} ({error}); start `gents runtime server` first")
        }
        Err(_) => {
            bail!("timed out reaching the Codex shim at {authority}; start `gents runtime server` first")
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cli::{Cli, Command as GentsCommand};
    use clap::Parser;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    fn args(remote: &str) -> CodexArgs {
        CodexArgs {
            remote: remote.to_string(),
            remote_auth_token_env: None,
            agent: None,
            no_alt_screen: false,
            prompt: None,
        }
    }

    pub(crate) fn identity(runnable_agents: Option<&[&str]>) -> RuntimeIdentity {
        RuntimeIdentity {
            node_did: "did:key:z".to_string(),
            home: PathBuf::from("/srv/home"),
            bound_agent: "a".to_string(),
            runnable_agents: runnable_agents
                .map(|agents| agents.iter().map(|agent| agent.to_string()).collect()),
            tool_root: Some(PathBuf::from("/srv/root")),
            default_thread_cwd: PathBuf::from("/srv/root/from-the-runtime"),
            grok_socket: None,
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
            codex_command_args(&args, None),
            vec![
                "--remote",
                crate::DEFAULT_CODEX_REMOTE,
                "--dangerously-bypass-approvals-and-sandbox",
                "--config",
                "show_raw_agent_reasoning=true",
                "--remote-auth-token-env",
                "GENTS_TOKEN",
                "--no-alt-screen",
                "--",
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

    #[test]
    fn agent_is_refused_for_wss_unix_and_non_loopback_ws() {
        for remote in [
            "wss://127.0.0.1:9292/",
            "wss://example.com/",
            "unix:///tmp/shim.sock",
            "ws://192.0.2.10:9292/",
        ] {
            let endpoint = resolve_endpoint(remote).expect("resolves");

            let error = agent_relay_upstream(&endpoint).expect_err(remote);

            assert!(error.to_string().contains("--agent"), "{remote}: {error:#}");
        }
    }

    #[test]
    fn agent_relays_to_a_loopback_websocket() {
        for (remote, authority) in [
            ("ws://127.0.0.1:9292/", "127.0.0.1:9292"),
            ("ws://localhost:9292/", "localhost:9292"),
            ("ws://[::1]:9292/", "[::1]:9292"),
        ] {
            let endpoint = resolve_endpoint(remote).expect("resolves");
            assert_eq!(agent_relay_upstream(&endpoint).expect(remote), authority);
        }
    }

    #[test]
    fn a_runnable_agent_goes() {
        assert_eq!(
            preflight_decision(&identity(Some(&["a", "b"])), "b"),
            PreflightDecision::Go
        );
    }

    #[test]
    fn an_empty_set_waits() {
        assert_eq!(
            preflight_decision(&identity(Some(&[])), "b"),
            PreflightDecision::WaitForPublish
        );
    }

    #[test]
    fn a_published_set_without_the_agent_names_gents_config_agent_list() {
        let PreflightDecision::Refuse(message) =
            preflight_decision(&identity(Some(&["a"])), "nosuch")
        else {
            panic!("an unknown agent is refused");
        };

        assert!(message.contains("nosuch"), "{message}");
        assert!(message.contains("gents config agent list"), "{message}");
    }

    #[test]
    fn a_pinned_runtime_says_agents_need_a_local_unpinned_runtime() {
        let PreflightDecision::Refuse(message) = preflight_decision(&identity(None), "b") else {
            panic!("a pinned runtime has no agent routes");
        };

        assert!(message.contains("--agent"), "{message}");
        assert!(message.contains("local"), "{message}");
        assert!(message.contains("pinned"), "{message}");
    }

    #[test]
    fn printed_commands_quote_a_home_with_spaces() {
        let mut other = identity(Some(&[]));
        other.home = PathBuf::from("/srv/other home");
        let refusal = foreign_endpoint_refusal("127.0.0.1:1", Some(&other));
        assert!(
            refusal.contains("`gents --home '/srv/other home'`"),
            "{refusal}"
        );
        let refusal = cannot_open_refusal(&other, Path::new("/srv/my home"));
        assert!(
            refusal.contains("gents status --home '/srv/my home'"),
            "{refusal}"
        );
    }

    #[tokio::test]
    async fn an_endpoint_that_cannot_identify_itself_gets_the_one_refusal() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let authority = closed.local_addr().unwrap().to_string();
        drop(closed);

        let error = preflight_agent(&authority, None, "b", Duration::ZERO)
            .await
            .expect_err("nothing answers");

        let error = error.to_string();
        assert!(!error.contains("does not belong"), "{error}");
        assert!(error.contains("could not confirm"), "{error}");
    }

    /// Answers each `/runtime` request with the next body, then keeps repeating the last.
    pub(crate) async fn runtime_answering(bodies: Vec<serde_json::Value>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        runtime_answering_on(listener, bodies);
        authority
    }

    pub(crate) fn runtime_answering_on(
        listener: tokio::net::TcpListener,
        bodies: Vec<serde_json::Value>,
    ) {
        tokio::spawn(async move {
            let mut bodies = bodies.into_iter().peekable();
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await;
                let body = if bodies.len() > 1 {
                    bodies.next().unwrap()
                } else {
                    bodies.peek().unwrap().clone()
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
    }

    fn published(agents: &[&str]) -> serde_json::Value {
        serde_json::to_value(identity(Some(agents))).unwrap()
    }

    #[tokio::test]
    async fn preflight_waits_for_the_first_publish() {
        let authority =
            runtime_answering(vec![published(&[]), published(&[]), published(&["b"])]).await;

        preflight_agent(&authority, None, "b", Duration::from_secs(30))
            .await
            .expect("the agent is published on the third look");
    }

    #[tokio::test]
    async fn preflight_stops_waiting_when_nothing_is_published() {
        let authority = runtime_answering(vec![published(&[])]).await;

        let error = preflight_agent(&authority, None, "b", Duration::from_millis(250))
            .await
            .expect_err("nothing is ever published");

        assert!(error.to_string().contains("gents status"), "{error:#}");
        assert!(!error.to_string().contains("unknown agent"), "{error:#}");
    }

    #[tokio::test]
    async fn preflight_refuses_an_unknown_agent_without_waiting() {
        let authority = runtime_answering(vec![published(&["a"])]).await;

        let error = preflight_agent(&authority, None, "b", Duration::from_secs(600))
            .await
            .expect_err("only an empty set waits");

        assert!(
            error.to_string().contains("gents config agent list"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn no_runnable_agent_says_so_after_the_wait_and_names_gents_status() {
        let authority = runtime_answering(vec![published(&[])]).await;

        let error = ensure_an_agent_can_run(
            &format!("ws://{authority}/"),
            Path::new("/srv/home"),
            false,
            Duration::from_millis(250),
        )
        .await
        .expect_err("nothing is ever published");

        let error = error.to_string();
        assert!(error.contains("no agent can run"), "{error}");
        let status = error.find("gents status --home /srv/home").expect(&error);
        let backend = error
            .find("gents config backend set --file <backend.json> --home /srv/home")
            .expect(&error);
        assert!(status < backend, "{error}");
    }

    #[tokio::test]
    async fn the_default_agent_has_to_be_runnable_when_no_agent_is_named() {
        let authority = runtime_answering(vec![published(&["other"])]).await;
        let remote = format!("ws://{authority}/");
        let home = Path::new("/srv/home");

        let error = ensure_an_agent_can_run(&remote, home, false, Duration::from_millis(250))
            .await
            .expect_err("only another agent can run");

        let error = error.to_string();
        assert!(error.contains(r#"default agent "a""#), "{error}");
        assert!(error.contains("gents --agent"), "{error}");
        assert!(error.contains("gents status --home /srv/home"), "{error}");
        ensure_an_agent_can_run(&remote, home, true, Duration::from_secs(600))
            .await
            .expect("a named agent gets its own check when it opens");
    }

    #[tokio::test]
    async fn a_named_agent_still_needs_something_runnable() {
        let authority = runtime_answering(vec![published(&[])]).await;

        let error = ensure_an_agent_can_run(
            &format!("ws://{authority}/"),
            Path::new("/srv/home"),
            true,
            Duration::from_millis(250),
        )
        .await
        .expect_err("nothing is ever published");

        assert!(error.to_string().contains("no agent can run"), "{error:#}");
    }

    #[tokio::test]
    async fn an_agent_published_during_the_wait_can_run() {
        let authority =
            runtime_answering(vec![published(&[]), published(&[]), published(&["a"])]).await;

        ensure_an_agent_can_run(
            &format!("ws://{authority}/"),
            Path::new("/srv/home"),
            false,
            Duration::from_secs(30),
        )
        .await
        .expect("an agent is published on the third look");
    }

    #[tokio::test]
    async fn a_pinned_runtime_skips_the_runnable_check_without_waiting() {
        let pinned = serde_json::to_value(identity(None)).unwrap();
        let authority = runtime_answering(vec![pinned]).await;

        ensure_an_agent_can_run(
            &format!("ws://{authority}/"),
            Path::new("/srv/home"),
            false,
            Duration::from_secs(600),
        )
        .await
        .expect("a pinned runtime publishes no set to check");
    }

    #[test]
    fn blank_agent_is_refused() {
        for arguments in [
            &["gents", "--agent", ""][..],
            &["gents", "--agent", "  "][..],
            &["gents", "codex", "--agent", ""][..],
            &["gents", "codex", "--agent", "  "][..],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn the_terminal_directory_is_passed_only_inside_the_tool_root() {
        let root = Path::new("/srv/root");

        assert_eq!(
            terminal_cd_arg(Path::new("/srv/root/project"), Some(root)),
            Some(PathBuf::from("/srv/root/project"))
        );
        assert_eq!(terminal_cd_arg(root, Some(root)), Some(root.to_path_buf()));
        assert_eq!(terminal_cd_arg(Path::new("/srv/other"), Some(root)), None);
        assert_eq!(terminal_cd_arg(Path::new("/srv/root2"), Some(root)), None);
        assert_eq!(terminal_cd_arg(Path::new("/srv/root"), None), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_terminal_inside_a_symlinked_tool_root_is_inside() {
        let temp = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(temp.path()).unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = real.with_file_name("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let inside = real.join("project");

        let root = canonical_tool_root(Some(link)).await;

        assert_eq!(
            terminal_cd_arg(&inside, root.as_deref()),
            Some(inside.clone())
        );
        let missing = PathBuf::from("/nonexistent/root");
        assert_eq!(
            canonical_tool_root(Some(missing.clone())).await,
            Some(missing)
        );
        assert_eq!(canonical_tool_root(None).await, None);
    }

    #[test]
    fn a_terminal_outside_the_tool_root_is_told_where_the_thread_starts() {
        let notice = outside_tool_root_notice(Path::new("/srv/other"), &identity(None))
            .expect("the terminal is outside the root");

        assert!(notice.contains("/srv/other"), "{notice}");
        assert!(notice.contains("/srv/root/from-the-runtime"), "{notice}");
        assert_eq!(notice.lines().count(), 1, "{notice}");
    }

    #[test]
    fn a_terminal_with_its_own_placement_or_no_root_hears_nothing() {
        for cwd in ["/srv/root", "/srv/root/project"] {
            assert_eq!(
                outside_tool_root_notice(Path::new(cwd), &identity(None)),
                None
            );
        }
        let mut no_root = identity(None);
        no_root.tool_root = None;
        assert_eq!(
            outside_tool_root_notice(Path::new("/srv/other"), &no_root),
            None
        );
    }

    #[test]
    fn the_directory_goes_before_the_prompt() {
        let mut args = args(crate::DEFAULT_CODEX_REMOTE);
        args.prompt = Some("hello".to_string());

        let command = codex_command_args(&args, Some(Path::new("/srv/root/project")));

        assert_eq!(
            command[command.len() - 4..],
            ["--cd", "/srv/root/project", "--", "hello"]
        );
    }
}
