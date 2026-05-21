#[path = "http/protocol.rs"]
mod protocol;
#[path = "http/routes.rs"]
mod routes;

use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;

use self::protocol::{read_http_request, write_http_response, HttpResponse};
use self::routes::handle_request;
use crate::live_fixture::LiveBridgeFixture;

pub(crate) struct BridgeRunnerServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BridgeRunnerServer {
    pub(crate) fn start(fixture: Arc<LiveBridgeFixture>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let runtime = fixture.runtime().handle().clone();
        let fixture_for_thread = Arc::clone(&fixture);
        let thread = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let response = match read_http_request(&mut stream).and_then(|request| {
                            handle_request(&runtime, &fixture_for_thread, request)
                        }) {
                            Ok(response) => response,
                            Err(error) => HttpResponse::json_error(
                                "500 Internal Server Error",
                                &error.to_string(),
                            ),
                        };
                        let _ = write_http_response(
                            &mut stream,
                            response.status,
                            response.content_type,
                            &response.body,
                        );
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            port,
            stop,
            handle: Some(thread),
        })
    }

    pub(crate) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
