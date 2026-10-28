use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use super::support::{self, ServeProcess};

fn reported_socket(readiness: &Value, configured: SocketAddr) -> Result<SocketAddr> {
    ensure!(
        configured.is_ipv4(),
        "this regression covers explicit IPv4 binding"
    );
    let rows = readiness["p2p_listen_addresses"]
        .as_array()
        .context("readiness lists P2P addresses")?;
    let peer = readiness["p2p_peer_id"]
        .as_str()
        .context("readiness names its P2P peer")?;
    let mut reported = BTreeSet::<SocketAddr>::new();
    for row in rows {
        let (id, hints) = p2p::iroh::parse_public_peer_addr(
            row.as_str().context("P2P listen address is a string")?,
        )
        .context("decoding the public P2P address")?;
        ensure!(
            id.as_str() == peer,
            "readiness address names another peer: {row}"
        );
        for hint in hints {
            reported.insert(
                hint.as_str()
                    .parse()
                    .with_context(|| format!("P2P dial hint is not a concrete socket: {hint}"))?,
            );
        }
    }
    ensure!(reported.len() == 1, "expected one P2P socket: {rows:?}");
    let actual = *reported.first().expect("exactly one reported socket");
    ensure!(
        actual.ip() == configured.ip(),
        "bind escaped {configured}: {actual}"
    );
    ensure!(
        actual.port() != 0,
        "P2P port must have been assigned: {actual}"
    );
    ensure!(
        configured.port() == 0 || actual.port() == configured.port(),
        "configured port changed: {configured} -> {actual}"
    );
    Ok(actual)
}

#[cfg(target_os = "macos")]
async fn observe_udp(pid: u32, configured: SocketAddr, expected: Option<SocketAddr>) -> Result<()> {
    let output = tokio::process::Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-iUDP", "-nP", "-Ftn"])
        .output()
        .await
        .context("observing the owned server's native UDP sockets")?;
    // Before endpoint startup, lsof exits with one when no UDP descriptors exist.
    if expected.is_none()
        && output.status.code() == Some(1)
        && output.stdout.is_empty()
        && output.stderr.is_empty()
    {
        return Ok(());
    }
    ensure!(
        output.status.success(),
        "lsof failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text =
        String::from_utf8(output.stdout).context("decoding native UDP socket observations")?;
    let mut family = None;
    let mut observed = false;
    for line in text.lines() {
        if line.starts_with('f') {
            family = None;
        }
        if let Some(value) = line.strip_prefix('t') {
            family = Some(value);
        }
        if let Some(address) = line.strip_prefix('n') {
            ensure!(
                family == Some("IPv4"),
                "explicit IPv4 bind opened another family: {text}"
            );
            // Route inspection may have an unbound descriptor or a connected
            // probe. A positive-port wildcard listener is a different exposure.
            if !address.contains("->") {
                let wildcard_port = address
                    .strip_prefix("*:")
                    .and_then(|port| port.parse::<u16>().ok());
                ensure!(
                    !wildcard_port.is_some_and(|port| port != 0),
                    "concrete bind {configured} opened a wildcard UDP listener: {text}"
                );
                if let Ok(socket) = address.parse::<SocketAddr>() {
                    ensure!(
                        socket.port() == 0 || socket.ip() == configured.ip(),
                        "concrete bind {configured} opened another UDP listener: {text}"
                    );
                }
            }
            observed |= address.parse::<SocketAddr>().ok() == expected;
        }
    }
    ensure!(
        expected.is_none() || observed,
        "configured P2P socket {expected:?} is absent from native sockets: {text}"
    );
    Ok(())
}

/// Observe the owned process from spawn through readiness and another two seconds,
/// so a short-lived gateway-discovery listener cannot hide behind advertised addresses.
#[cfg(target_os = "macos")]
pub async fn spawn_observed_server(
    home: &Path,
    port: u16,
    args: &[&str],
    configured: SocketAddr,
) -> Result<(ServeProcess, Value)> {
    use std::time::Duration;
    const LOG: &str = "warn,p2p::iroh::endpoint_config=info";
    let registered_home = support::process::served_home_dir(home, args, &[("RUST_LOG", LOG)]);
    let owned_home = home.to_owned();
    let owned_args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let mut serve = tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();
        support::spawn_server_with_env(&owned_home, port, &args, &[("RUST_LOG", LOG)])
    })
    .await??;
    let stdout = serve
        .stdout_log
        .as_ref()
        .context("server stdout capture")?
        .path()
        .to_owned();
    let stderr = serve
        .stderr_log
        .as_ref()
        .context("server stderr capture")?
        .path()
        .to_owned();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut startup_samples = 0;
    let readiness = loop {
        observe_udp(serve.child.id(), configured, None).await?;
        startup_samples += 1;
        let output = tokio::fs::read_to_string(&stdout).await?;
        if let Some(value) = support::process::server_readiness_json(&output) {
            break value;
        }
        ensure!(
            serve.child.try_wait()?.is_none(),
            "server exited during bind observation: {}",
            tokio::fs::read_to_string(&stderr).await?
        );
        ensure!(
            tokio::time::Instant::now() < deadline,
            "server readiness timed out: {}",
            tokio::fs::read_to_string(&stderr).await?
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    tokio::task::spawn_blocking(move || {
        support::graphql::register_served_home(&registered_home, port)
    })
    .await??;
    let actual = reported_socket(&readiness, configured)?;
    let finish = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut ready_samples = 0;
    while tokio::time::Instant::now() < finish {
        observe_udp(serve.child.id(), configured, Some(actual)).await?;
        ready_samples += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ensure!(
        startup_samples > 0 && ready_samples > 1,
        "both startup and ready native scopes must be observed"
    );
    let diagnostic = tokio::fs::read_to_string(stderr).await?;
    ensure!(diagnostic.contains("automatic port mapping disabled: gateway discovery cannot be scoped to the selected address"),
        "the concrete-bind port-mapping diagnostic is absent: {diagnostic}");
    Ok((serve, readiness))
}

#[cfg(not(target_os = "macos"))]
pub async fn spawn_observed_server(
    home: &Path,
    port: u16,
    args: &[&str],
    configured: SocketAddr,
) -> Result<(ServeProcess, Value)> {
    let home = home.to_owned();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let (serve, readiness) = tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        support::spawn_server_with_ready_json(&home, port, &args, &[])
    })
    .await??;
    reported_socket(&readiness, configured)?;
    Ok((serve, readiness))
}
