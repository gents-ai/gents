use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use url::Url;

const MAX_HEAD_BYTES: usize = 64 * 1024;
const HEAD_END: &[u8] = b"\r\n\r\n";
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Codex refuses a path or query in `--remote`, so a loopback listener puts the
/// agent's route on each upgrade request and forwards the connection to the shim.
///
/// It adds no authority: every header, the bearer included, reaches the shim
/// unchanged, and the shim decides. It prints nothing, because bare `gents` has no
/// tracing subscriber and any output would land on the terminal interface. A
/// connection that fails just closes, and Codex reports the failed handshake.
pub(crate) struct AgentRelay {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl AgentRelay {
    pub(crate) async fn bind(upstream: String, agent_id: &str) -> Result<AgentRelay> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("binding the agent relay")?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(serve(listener, upstream, agent_route_target(agent_id)));
        Ok(AgentRelay { addr, task })
    }

    pub(crate) fn remote(&self) -> String {
        format!("ws://{}", self.addr)
    }
}

impl Drop for AgentRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Aborting this drops the `JoinSet`, which aborts every open connection with it.
async fn serve(listener: TcpListener, upstream: String, target: String) {
    let mut connections = JoinSet::new();
    loop {
        let accepted = listener.accept().await;
        while connections.try_join_next().is_some() {}
        match accepted {
            Ok((client, _)) => {
                connections.spawn(relay_connection(client, upstream.clone(), target.clone()));
            }
            // Most likely out of descriptors, where retrying at once would spin.
            Err(_) => tokio::time::sleep(ACCEPT_RETRY).await,
        }
    }
}

async fn relay_connection(mut client: TcpStream, upstream: String, target: String) -> Result<()> {
    let (received, head_len) = read_head(&mut client).await?;
    let head = rewrite_request_target(&received[..head_len], &target)?;
    let mut server = TcpStream::connect(&upstream).await?;
    server.write_all(&head).await?;
    server.write_all(&received[head_len..]).await?;
    tokio::io::copy_bidirectional(&mut client, &mut server).await?;
    Ok(())
}

/// Returns everything read so far and how much of it is the head: a client may send
/// its first bytes along with the head, and those belong to the connection.
async fn read_head(client: &mut TcpStream) -> Result<(Vec<u8>, usize)> {
    let mut received = Vec::with_capacity(4096);
    loop {
        if let Some(at) = received
            .windows(HEAD_END.len())
            .position(|window| window == HEAD_END)
        {
            let head_len = at + HEAD_END.len();
            ensure!(head_len <= MAX_HEAD_BYTES, "request head over the limit");
            return Ok((received, head_len));
        }
        ensure!(
            received.len() <= MAX_HEAD_BYTES,
            "request head over the limit"
        );
        ensure!(
            client.read_buf(&mut received).await? > 0,
            "connection closed before its request head ended"
        );
    }
}

fn agent_route_target(agent_id: &str) -> String {
    let mut route = Url::parse("ws://relay/agents").expect("a constant URL parses");
    route
        .path_segments_mut()
        .expect("a ws URL has path segments")
        .push(agent_id);
    route.path().to_string()
}

fn rewrite_request_target(head: &[u8], target: &str) -> Result<Vec<u8>> {
    let line_end = head
        .windows(2)
        .position(|pair| pair == b"\r\n")
        .unwrap_or(head.len());
    let line = &head[..line_end];
    let (Some(start), Some(end)) = (
        line.iter().position(|&byte| byte == b' '),
        line.iter().rposition(|&byte| byte == b' '),
    ) else {
        bail!("request line has no target");
    };
    ensure!(start < end, "request line has no target");
    Ok([&head[..=start], target.as_bytes(), &head[end..]].concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(10);
    const HEAD: &[u8] = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:1\r\nAuthorization: Bearer the secret\r\nUpgrade: websocket\r\n\r\n";

    async fn within<F>(future: F) -> F::Output
    where
        F: std::future::Future,
    {
        tokio::time::timeout(WAIT, future)
            .await
            .expect("the relay should have answered by now")
    }

    /// Reports each connection's first bytes, then echoes everything after the head.
    fn echo_on(listener: TcpListener) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (heads_tx, heads) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let heads_tx = heads_tx.clone();
                tokio::spawn(async move {
                    let mut received = Vec::new();
                    let head_len = loop {
                        if let Some(at) = received
                            .windows(HEAD_END.len())
                            .position(|window| window == HEAD_END)
                        {
                            break at + HEAD_END.len();
                        }
                        if stream.read_buf(&mut received).await.unwrap_or(0) == 0 {
                            return;
                        }
                    };
                    let _ = heads_tx.send(received[..head_len].to_vec());
                    let _ = stream.write_all(&received[head_len..]).await;
                    let (mut reader, mut writer) = stream.split();
                    let _ = tokio::io::copy(&mut reader, &mut writer).await;
                });
            }
        });
        heads
    }

    async fn echo_upstream() -> (String, mpsc::UnboundedReceiver<Vec<u8>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        (addr, echo_on(listener))
    }

    fn authority(relay: &AgentRelay) -> String {
        relay.remote().trim_start_matches("ws://").to_string()
    }

    async fn upgraded(relay: &AgentRelay) -> TcpStream {
        let mut client = within(TcpStream::connect(authority(relay))).await.unwrap();
        client.write_all(HEAD).await.unwrap();
        client
    }

    async fn round_trip(client: &mut TcpStream, text: &[u8]) {
        client.write_all(text).await.unwrap();
        let mut back = vec![0; text.len()];
        within(client.read_exact(&mut back)).await.unwrap();
        assert_eq!(back, text);
    }

    async fn assert_closed(client: &mut TcpStream) {
        let mut byte = [0; 1];
        let read = within(client.read(&mut byte)).await;
        assert!(matches!(read, Ok(0) | Err(_)), "still open: {read:?}");
    }

    #[test]
    fn agent_ids_encode_as_one_segment() {
        for (agent, target) in [
            ("did:key:z6Mk:default", "/agents/did:key:z6Mk:default"),
            ("a/b", "/agents/a%2Fb"),
            ("a b", "/agents/a%20b"),
        ] {
            assert_eq!(agent_route_target(agent), target);
        }
    }

    #[test]
    fn rewrite_changes_only_the_target() {
        let rewritten = rewrite_request_target(HEAD, "/agents/a%2Fb").unwrap();

        assert_eq!(
            rewritten,
            b"GET /agents/a%2Fb HTTP/1.1\r\nHost: 127.0.0.1:1\r\nAuthorization: Bearer the secret\r\nUpgrade: websocket\r\n\r\n"
        );
        assert!(rewrite_request_target(b"nonsense\r\n\r\n", "/agents/x").is_err());
    }

    #[tokio::test]
    async fn frames_cross_both_ways() {
        let (upstream, mut heads) = echo_upstream().await;
        let relay = AgentRelay::bind(upstream, "a/b").await.unwrap();
        let mut client = within(TcpStream::connect(authority(&relay))).await.unwrap();

        // Bytes that arrive with the head belong to the connection, not the head.
        client.write_all(&[HEAD, b"early"].concat()).await.unwrap();
        let mut early = [0; 5];
        within(client.read_exact(&mut early)).await.unwrap();
        assert_eq!(&early, b"early");
        round_trip(&mut client, b"frame").await;

        let head = within(heads.recv()).await.unwrap();
        assert_eq!(
            head,
            rewrite_request_target(HEAD, "/agents/a%2Fb").unwrap(),
            "the shim must see the route and every original header"
        );
    }

    #[tokio::test]
    async fn a_head_over_the_limit_closes_that_connection_only() {
        let (upstream, mut heads) = echo_upstream().await;
        let relay = AgentRelay::bind(upstream, "x").await.unwrap();
        let padding = "a".repeat(MAX_HEAD_BYTES);
        let complete = format!("GET / HTTP/1.1\r\nX-Padding: {padding}\r\n\r\n");

        for oversized in [vec![b'a'; MAX_HEAD_BYTES + 1], complete.into_bytes()] {
            let mut client = within(TcpStream::connect(authority(&relay))).await.unwrap();
            // The relay may close while the last bytes are still on their way.
            let _ = client.write_all(&oversized).await;
            assert_closed(&mut client).await;
        }
        assert!(heads.try_recv().is_err(), "nothing may reach the shim");

        let mut next = upgraded(&relay).await;
        round_trip(&mut next, b"frame").await;
    }

    #[tokio::test]
    async fn it_survives_a_failed_upstream_and_serves_the_next_connection() {
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let relay = AgentRelay::bind(addr.to_string(), "x").await.unwrap();

        let mut failed = upgraded(&relay).await;
        assert_closed(&mut failed).await;

        let _heads = echo_on(TcpListener::bind(addr).await.unwrap());
        let mut next = upgraded(&relay).await;
        round_trip(&mut next, b"frame").await;
    }

    #[tokio::test]
    async fn two_connections_run_at_once() {
        let (upstream, _heads) = echo_upstream().await;
        let relay = AgentRelay::bind(upstream, "x").await.unwrap();
        let mut stalled = within(TcpStream::connect(authority(&relay))).await.unwrap();
        stalled.write_all(b"GET / HT").await.unwrap();

        let mut first = upgraded(&relay).await;
        let mut second = upgraded(&relay).await;

        round_trip(&mut first, b"one").await;
        round_trip(&mut second, b"two").await;
        round_trip(&mut first, b"three").await;
    }

    #[tokio::test]
    async fn dropping_the_relay_stops_it() {
        let (upstream, _heads) = echo_upstream().await;
        let relay = AgentRelay::bind(upstream, "x").await.unwrap();
        let address = authority(&relay);
        let mut open = upgraded(&relay).await;
        round_trip(&mut open, b"frame").await;

        drop(relay);

        assert_closed(&mut open).await;
        within(async {
            while TcpStream::connect(&address).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
}
