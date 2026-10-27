use std::collections::BTreeSet;
use std::net::SocketAddr;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

/// Iroh readiness also includes an endpoint id and ticket. Its socket entries
/// must stay inside the requested bind scope; advertised addresses alone cannot
/// establish that the process has no additional wildcard listener.
pub async fn assert_ipv4_bind_scope(
    pid: u32,
    readiness: &Value,
    configured: SocketAddr,
) -> Result<()> {
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

    #[cfg(target_os = "macos")]
    {
        let output = tokio::process::Command::new("/usr/sbin/lsof")
            .args(["-a", "-p", &pid.to_string(), "-iUDP", "-nP", "-Ftn"])
            .output()
            .await
            .context("observing the owned server's native UDP sockets")?;
        ensure!(
            output.status.success(),
            "lsof failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text =
            String::from_utf8(output.stdout).context("decoding native UDP socket observations")?;
        let mut family = None;
        let mut observed = false;
        // UPnP gateway discovery also opens an IPv4 UDP socket. Its bind scope
        // is independent of the QUIC transport; no native IPv6 socket is allowed
        // by this explicit IPv4 regression, even if it is absent from readiness.
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
                observed |= address
                    .parse::<SocketAddr>()
                    .is_ok_and(|address| address == actual);
            }
        }
        ensure!(
            observed,
            "configured P2P socket {actual} is absent from the native sockets: {text}"
        );
    }
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
    Ok(())
}
