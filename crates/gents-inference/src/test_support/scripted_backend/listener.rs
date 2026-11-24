use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use rapidhash::RapidHashMap;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

/// The connections the backend holds open, by peer, so a scripted response
/// can end the one its request arrived on.
#[derive(Clone, Default)]
pub(super) struct Connections(Arc<Mutex<RapidHashMap<SocketAddr, Arc<AtomicBool>>>>);

impl Connections {
    /// Every later read or write on the peer's connection fails, so the
    /// server drops it without answering.
    pub(super) fn close(&self, peer: SocketAddr) {
        if let Some(closed) = self.lock().get(&peer) {
            closed.store(true, Ordering::SeqCst);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RapidHashMap<SocketAddr, Arc<AtomicBool>>> {
        self.0
            .lock()
            .expect("scripted backend connections mutex poisoned")
    }
}

pub(super) struct TrackedListener {
    inner: TcpListener,
    connections: Connections,
}

impl TrackedListener {
    pub(super) fn new(inner: TcpListener, connections: Connections) -> Self {
        Self { inner, connections }
    }
}

impl Listener for TrackedListener {
    type Io = TrackedStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, peer) = Listener::accept(&mut self.inner).await;
        let closed = Arc::new(AtomicBool::new(false));
        self.connections.lock().insert(peer, closed.clone());
        let stream = TrackedStream {
            inner: stream,
            peer,
            closed,
            connections: self.connections.clone(),
        };
        (stream, peer)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

pub(super) struct TrackedStream {
    inner: TcpStream,
    peer: SocketAddr,
    closed: Arc<AtomicBool>,
    connections: Connections,
}

impl TrackedStream {
    fn check(&self) -> io::Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "scripted backend closed the connection",
            ));
        }
        Ok(())
    }
}

impl Drop for TrackedStream {
    fn drop(&mut self) {
        self.connections.lock().remove(&self.peer);
    }
}

impl AsyncRead for TrackedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check()?;
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TrackedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check()?;
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check()?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The peer a request arrived from.
#[derive(Clone, Copy, Debug)]
pub(super) struct PeerAddr(pub(super) SocketAddr);

impl Connected<IncomingStream<'_, TrackedListener>> for PeerAddr {
    fn connect_info(stream: IncomingStream<'_, TrackedListener>) -> Self {
        Self(*stream.remote_addr())
    }
}
