use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

use super::{read_http_request, write_http_response};

pub struct MockModelEndpoint {
    pub endpoint: String,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub handle: Option<JoinHandle<()>>,
}

impl MockModelEndpoint {
    pub fn start(model_name: &str) -> Result<Self> {
        Self::start_with_required_bearer(model_name, None)
    }

    pub fn start_with_required_bearer(
        model_name: &str,
        required_bearer: Option<&str>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding mock model port")?;
        listener
            .set_nonblocking(true)
            .context("marking mock model listener nonblocking")?;
        let port = listener
            .local_addr()
            .context("reading mock model port")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let required_bearer = required_bearer.map(ToOwned::to_owned);
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };

                        let authorized = required_bearer.as_ref().is_none_or(|expected| {
                            request
                                .headers
                                .get("authorization")
                                .is_some_and(|value| value == &format!("Bearer {expected}"))
                        });
                        let (status, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            if authorized {
                                ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
                            } else {
                                (
                                    "401 Unauthorized",
                                    r#"{"error":"unauthorized"}"#.to_string(),
                                )
                            }
                        } else {
                            ("404 Not Found", r#"{"error":"not found"}"#.to_string())
                        };
                        let _ = write_http_response(&mut stream, status, "application/json", &body);
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
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            handle: Some(handle),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
