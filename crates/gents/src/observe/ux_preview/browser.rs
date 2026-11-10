//! The browser under a preview: a system Chrome, Chromium or Edge, headless,
//! driven over the DevTools protocol. The browser itself listens for
//! DevTools on an ephemeral loopback port (`--remote-debugging-port=0`) for
//! the life of the preview, so another local process that finds the port can
//! drive it; `--remote-debugging-pipe` would close that and is not used yet.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures::{SinkExt, StreamExt};
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

const COMMAND_DEADLINE: Duration = Duration::from_secs(45);
const LAUNCH_DEADLINE: Duration = Duration::from_secs(20);
const CLOSE_DEADLINE: Duration = Duration::from_secs(5);
const LISTENING_PREFIX: &str = "DevTools listening on ";

const BROWSER_APPS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];
const BROWSER_COMMANDS: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "microsoft-edge-stable",
    "chrome",
    "msedge",
];

/// The first system browser this machine has.
pub(super) fn find() -> Result<PathBuf> {
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|path| {
        std::env::split_paths(&path)
            .flat_map(|dir| BROWSER_COMMANDS.iter().map(move |name| dir.join(name)))
            .collect::<Vec<_>>()
    });
    BROWSER_APPS
        .iter()
        .map(PathBuf::from)
        .chain(on_path)
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "ux preview needs a system browser and found none; install Google Chrome, Chromium or Microsoft Edge (looked in /Applications and on PATH for {})",
                BROWSER_COMMANDS.join(", ")
            )
        })
}

/// One DevTools event, with the session it came from.
pub(super) struct Event {
    pub method: String,
    pub params: Value,
}

type Pending = Arc<Mutex<RapidHashMap<u64, oneshot::Sender<Value>>>>;

/// A DevTools connection: commands and their replies. Events arrive on the
/// receiver [`Self::connect`] returns, in order, while commands run.
pub(super) struct Cdp {
    commands: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
}

impl Cdp {
    async fn connect(url: &str) -> Result<(Self, mpsc::UnboundedReceiver<Event>)> {
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .context("connecting to the browser's DevTools socket")?;
        let (mut sink, mut stream) = socket.split();
        let (commands, mut outbound) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(text) = outbound.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::new(Mutex::new(RapidHashMap::new()));
        let replies = pending.clone();
        let (events, received) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    let waiter = replies
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&id);
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(value);
                    }
                } else if let Some(method) = value.get("method").and_then(Value::as_str) {
                    let _ = events.send(Event {
                        method: method.to_owned(),
                        params: value.get("params").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            replies
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        });
        Ok((
            Self {
                commands,
                pending,
                next_id: AtomicU64::new(1),
            },
            received,
        ))
    }

    /// Runs one command, on `session` when given, and answers its result.
    pub(super) async fn send(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (waiter, reply) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, waiter);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.commands
            .send(message.to_string())
            .map_err(|_| anyhow!("{method}: the browser connection closed"))?;
        let reply = tokio::time::timeout(COMMAND_DEADLINE, reply)
            .await
            .map_err(|_| anyhow!("{method}: the browser did not answer"))?
            .map_err(|_| anyhow!("{method}: the browser connection closed"))?;
        if let Some(error) = reply.get("error") {
            bail!(
                "{method}: {}",
                error["message"].as_str().unwrap_or("failed")
            );
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// A launched browser with its profile; dropping it kills the process.
pub(super) struct Browser {
    pub(super) cdp: Arc<Cdp>,
    pub(super) events: Option<mpsc::UnboundedReceiver<Event>>,
    child: Child,
    _profile: tempfile::TempDir,
}

impl Browser {
    pub(super) async fn launch(path: &Path) -> Result<Self> {
        let profile = tempfile::tempdir().context("creating the browser profile")?;
        let mut child = Command::new(path)
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--hide-scrollbars",
                // keeps the sandboxed frame in a process of its own, the path
                // the frame's DevTools session (`Target.setAutoAttach`) rides
                "--enable-features=IsolateSandboxedIframes",
                "about:blank",
            ])
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("launching {}", path.display()))?;
        let mut lines = BufReader::new(child.stderr.take().context("browser stderr")?).lines();
        let endpoint = tokio::time::timeout(LAUNCH_DEADLINE, async {
            while let Some(line) = lines.next_line().await? {
                if let Some(url) = line.strip_prefix(LISTENING_PREFIX) {
                    return Ok(url.trim().to_owned());
                }
            }
            bail!("the browser exited before opening DevTools")
        })
        .await
        .context("the browser did not open DevTools in time")??;
        // A browser blocks once its stderr pipe fills; keep it drained.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let (cdp, events) = Cdp::connect(&endpoint).await?;
        Ok(Self {
            cdp: Arc::new(cdp),
            events: Some(events),
            child,
            _profile: profile,
        })
    }

    /// Asks the browser to quit and waits for it; a browser that refuses is
    /// killed when this value drops.
    pub(super) async fn close(mut self) {
        let _ = self.cdp.send(None, "Browser.close", json!({})).await;
        let _ = tokio::time::timeout(CLOSE_DEADLINE, self.child.wait()).await;
    }
}
