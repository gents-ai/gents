use crate::support::*;

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::Message;

pub(super) const HTTP_PORT: u16 = 21919;
pub(super) const SHIM_PORT: u16 = 21929;
pub(super) const NO_MODEL_ENDPOINT: &str = "http://127.0.0.1:9/v1";
pub(super) const NO_MODEL_NAME: &str = "no-model";
pub(super) const MISSING_BIN: &str = "/nonexistent/gents-live-interface";
pub(super) const INSTALL_MESSAGE: &str = "Codex was not found";
/// Longer than the 60 s `gents` itself waits for the endpoint.
pub(super) const RUNTIME_UP: Duration = Duration::from_secs(90);
pub(super) const EXIT: Duration = Duration::from_secs(10);
pub(super) const PROMPT: Duration = Duration::from_secs(30);
pub(super) const TURN: Duration = Duration::from_secs(240);
pub(super) const FOLDER_QUESTION: &str = "Data folder [";
pub(super) const INFERENCE_QUESTION: &str = "How do you want to run inference?";
pub(super) const TOOLS_QUESTION: &str = "What may agents do?";
pub(super) const ENDPOINT_TEXT: &str = "127.0.0.1:21929";
pub(super) const OPENS_THREAD: Duration = Duration::from_secs(60);
pub(super) const OUTSIDE_THE_ROOT_TEXT: &str = "the thread will use";

/// One test's temporary user. Owns the launchd job from the moment it exists,
/// and every refusal happens before that, so `Drop` never removes a job this
/// run did not load.
pub(super) struct LiveBox {
    _temp: tempfile::TempDir,
    pub(super) root: PathBuf,
    label: String,
    domain: String,
    homes: Mutex<Vec<PathBuf>>,
}

pub(super) struct InitializedHome {
    pub(super) home: PathBuf,
    pub(super) node_did: String,
    pub(super) default_agent_id: String,
    pub(super) profile_id: String,
}

fn job_loaded(target: &str) -> Result<bool> {
    Ok(Command::new("/bin/launchctl")
        .args(["print", target])
        .output()
        .context("running launchctl print")?
        .status
        .success())
}

/// The safety boundary: this binary built without `native-e2e` addresses the
/// user's daily service, so nothing may reach launchd until the build's own
/// `service update-check` report has passed this.
pub(super) fn isolated_label(metadata: &Value) -> Result<&str> {
    let os = metadata["os"].as_str().unwrap_or_default();
    ensure!(
        os == "macos",
        "this suite drives launchd; refusing on {os:?}"
    );
    let label = metadata["service_label"].as_str().unwrap_or_default();
    ensure!(
        label.ends_with(".native-e2e"),
        "refusing to address the daily runtime {label:?}; run through `make live-onboarding`"
    );
    Ok(label)
}

impl LiveBox {
    pub(super) fn new() -> Result<Self> {
        let temp = tempfile::tempdir().context("creating the temporary HOME")?;
        let root = fs::canonicalize(temp.path())?;
        let output = Command::new(cli_bin())
            .env("HOME", &root)
            .args(["service", "update-check"])
            .output()
            .context("running gents service update-check")?;
        ensure!(
            output.status.success(),
            "gents service update-check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Value = serde_json::from_slice(&output.stdout)?;
        let label = isolated_label(&metadata)?;

        let uid = Command::new("/usr/bin/id").arg("-u").output()?;
        ensure!(uid.status.success(), "could not identify the user domain");
        let domain = format!("gui/{}", String::from_utf8(uid.stdout)?.trim());
        let target = format!("{domain}/{label}");
        ensure!(
            !job_loaded(&target)?,
            "{label} is already loaded; run `launchctl bootout {target}`"
        );
        for port in [HTTP_PORT, SHIM_PORT] {
            ensure!(
                std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
                "port {port} is in use; if a killed run left it, run `launchctl bootout {target}`"
            );
        }

        let codex_home = root.join("codex-home");
        fs::create_dir_all(&codex_home)?;
        // The first window caches the release it saw, and the next one would stop
        // at Codex's update screen before opening a thread.
        fs::write(
            codex_home.join("config.toml"),
            "# Existing user Codex config should remain client-side.\n\
             check_for_update_on_startup = false\n",
        )?;
        Ok(Self {
            _temp: temp,
            root,
            label: label.to_string(),
            domain,
            homes: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn target(&self) -> String {
        format!("{}/{}", self.domain, self.label)
    }

    pub(super) fn gents(&self) -> Command {
        let mut command = Command::new(cli_bin());
        command
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("CODEX_HOME", self.root.join("codex-home"))
            .env("TERM", "xterm-256color")
            .env("RUST_LOG", "error")
            .env_remove("GENTS_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("GENTS_CODEX_BIN")
            .env_remove("GENTS_GROK_BIN")
            .env_remove("INFERENCE_ENDPOINT");
        command
    }

    /// A folder under the temporary `HOME` whose service `Drop` uninstalls.
    pub(super) fn folder(&self, name: &str) -> PathBuf {
        let folder = self.root.join(name);
        self.homes.lock().expect("homes lock").push(folder.clone());
        folder
    }

    pub(super) fn log(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.terminal.log"))
    }

    pub(super) fn remembered_home(&self) -> PathBuf {
        self.root.join("Library/Application Support/gents/home")
    }

    /// Bare `gents` choosing file store-key custody, as it would on a first run.
    pub(super) fn first_run(&self, interface: &str) -> Command {
        let mut command = self.gents();
        command
            .env("GENTS_CODEX_BIN", interface)
            .args(["--store-key-custody", "file"]);
        command
    }

    pub(super) fn init_home(
        &self,
        name: &str,
        endpoint: &str,
        model: &str,
    ) -> Result<InitializedHome> {
        let home = self.folder(name);
        let output = self
            .gents()
            .args(["init", "--store-key-custody", "file", "--home"])
            .arg(&home)
            .args(["--inference-url", endpoint, "--model-name", model])
            .output()
            .context("running gents init")?;
        ensure!(
            output.status.success(),
            "gents init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let init: Value = serde_json::from_slice(&output.stdout)?;
        Ok(InitializedHome {
            home,
            node_did: node_did_from_init(&init)?,
            default_agent_id: init["default_agent_id"]
                .as_str()
                .context("init reports a default agent")?
                .to_string(),
            profile_id: init["inference_profile_id"]
                .as_str()
                .context("init reports an inference profile")?
                .to_string(),
        })
    }

    /// Clones `home`'s default agent, with no restart, and waits for its publish.
    pub(super) async fn clone_agent(
        &self,
        home: &InitializedHome,
        display_name: &str,
    ) -> Result<String> {
        let output = self
            .gents()
            .args(["config", "agent", "clone", &home.default_agent_id, "--home"])
            .arg(&home.home)
            .args(["--graphql", &graphql_url(HTTP_PORT)])
            .args(["--display-name", display_name])
            .args(["--profile-id", &home.profile_id])
            .output()
            .context("running gents config agent clone")?;
        ensure!(
            output.status.success(),
            "gents config agent clone failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cloned: Value = serde_json::from_slice(&output.stdout)?;
        let agent = cloned["agent_id"]
            .as_str()
            .context("clone reports its agent")?
            .to_string();
        let deadline = Instant::now() + PROMPT;
        loop {
            let runtime: Value = reqwest::Client::builder()
                .no_proxy()
                .build()?
                .get(format!("http://127.0.0.1:{SHIM_PORT}/runtime"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if runtime["runnable_agents"]
                .as_array()
                .is_some_and(|agents| agents.iter().any(|listed| listed == &agent))
            {
                return Ok(agent);
            }
            ensure!(
                Instant::now() < deadline,
                "{agent} was never published: {runtime}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// `gents --home <home>`, opening `interface` as its Codex binary.
    pub(super) fn bare_gents(&self, interface: &str, home: &Path) -> Command {
        let mut command = self.gents();
        command
            .env("GENTS_CODEX_BIN", interface)
            .arg("--home")
            .arg(home);
        command
    }

    /// `gents --home <home>`, opening `interface` as its Grok binary.
    pub(super) fn bare_gents_grok(&self, interface: &str, home: &Path) -> Command {
        let mut command = self.gents();
        command
            .env("GENTS_GROK_BIN", interface)
            .arg("--home")
            .arg(home);
        command
    }

    /// Brings up `home`'s service through bare `gents`, which then stops at the
    /// missing interface.
    pub(super) async fn bring_up(&self, home: &Path) -> Result<()> {
        let mut terminal =
            Terminal::spawn(&self.bare_gents(MISSING_BIN, home), &self.log("bring-up"))?;
        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect_exit(1).await
    }

    pub(super) fn gents_ok(&self, args: &[&str]) -> Result<()> {
        let output = self
            .gents()
            .args(args)
            .output()
            .with_context(|| format!("running gents {args:?}"))?;
        ensure!(
            output.status.success(),
            "gents {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    pub(super) fn service_pid(&self) -> Option<u32> {
        let output = Command::new("/bin/launchctl")
            .args(["print", &self.target()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = ")?.parse().ok())
    }

    pub(super) fn plist(&self) -> PathBuf {
        self.root
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", self.label))
    }

    pub(super) fn plist_mtime(&self) -> Result<SystemTime> {
        Ok(fs::metadata(self.plist())?.modified()?)
    }

    pub(super) fn label_disabled(&self) -> Result<bool> {
        let output = Command::new("/bin/launchctl")
            .args(["print-disabled", &self.domain])
            .output()
            .context("running launchctl print-disabled")?;
        ensure!(output.status.success(), "launchctl print-disabled failed");
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_once("=>"))
            .any(|(label, value)| {
                label.trim().trim_matches('"') == self.label
                    && matches!(value.trim(), "disabled" | "true")
            }))
    }
}

impl Drop for LiveBox {
    fn drop(&mut self) {
        for home in self.homes.lock().expect("homes lock").iter() {
            let _ = self
                .gents()
                .args(["service", "uninstall", "--home"])
                .arg(home)
                .output();
        }
        let target = self.target();
        if job_loaded(&target).unwrap_or(true) {
            let _ = Command::new("/bin/launchctl")
                .args(["bootout", &target])
                .output();
        }
    }
}

/// The five queries `write_expect_smoke` answers for Codex's terminal startup.
const TERMINAL_QUERIES: [(&[u8], &[u8]); 5] = [
    (b"\x1b[6n", b"\x1b[24;1R"),
    (b"\x1b[?u", b"\x1b[?0u"),
    (b"\x1b[c", b"\x1b[?1;2c"),
    (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
    (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
];

/// A child on a private 40 by 120 PTY, with everything it writes kept in order.
pub(super) struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    transcript: Arc<Mutex<Vec<u8>>>,
    drained: Arc<AtomicBool>,
    cursor: usize,
    _master: Box<dyn MasterPty + Send>,
}

impl Terminal {
    pub(super) fn spawn(command: &Command, log: &Path) -> Result<Self> {
        let mut builder = CommandBuilder::new(command.get_program());
        builder.args(command.get_args());
        if let Some(directory) = command.get_current_dir() {
            builder.cwd(directory.as_os_str());
        }
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => builder.env(key, value),
                None => builder.env_remove(key),
            }
        }
        let mut log = File::create(log)?;
        let pair = portable_pty::native_pty_system().openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut reader = pair.master.try_clone_reader()?;
        let input = Arc::new(Mutex::new(pair.master.take_writer()?));
        let transcript = Arc::new(Mutex::new(Vec::new()));
        let drained = Arc::new(AtomicBool::new(false));
        let replies = Arc::downgrade(&input);
        let output = transcript.clone();
        let finished = drained.clone();
        std::thread::Builder::new()
            .name("live-onboarding-terminal".into())
            .spawn(move || {
                let mut matched = [0_usize; TERMINAL_QUERIES.len()];
                let mut buffer = [0_u8; 8192];
                loop {
                    let read = match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => read,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let _ = log.write_all(&buffer[..read]);
                    output
                        .lock()
                        .expect("transcript lock")
                        .extend_from_slice(&buffer[..read]);
                    for &byte in &buffer[..read] {
                        for (index, (query, reply)) in TERMINAL_QUERIES.iter().enumerate() {
                            if byte != query[matched[index]] {
                                matched[index] = usize::from(byte == query[0]);
                                continue;
                            }
                            matched[index] += 1;
                            if matched[index] == query.len() {
                                matched[index] = 0;
                                if let Some(input) = replies.upgrade() {
                                    let mut input = input.lock().expect("terminal input lock");
                                    let _ = input.write_all(reply);
                                    let _ = input.flush();
                                }
                            }
                        }
                    }
                }
                finished.store(true, Ordering::SeqCst);
            })?;
        let child = pair
            .slave
            .spawn_command(builder)
            .context("starting the terminal's child")?;
        drop(pair.slave);
        Ok(Self {
            child,
            input,
            transcript,
            drained,
            cursor: 0,
            _master: pair.master,
        })
    }

    pub(super) fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.transcript.lock().expect("transcript lock")).into_owned()
    }

    /// Waits for `needle` after the previous match, so a run of calls asserts order.
    pub(super) async fn expect(&mut self, needle: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let found = {
                let transcript = self.transcript.lock().expect("transcript lock");
                transcript[self.cursor..]
                    .windows(needle.len())
                    .position(|window| window == needle.as_bytes())
            };
            if let Some(position) = found {
                self.cursor += position + needle.len();
                return Ok(());
            }
            if self.drained.load(Ordering::SeqCst) {
                bail!(
                    "the terminal closed before {needle:?}; transcript:\n{}",
                    self.transcript()
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out after {timeout:?} waiting for {needle:?}; transcript:\n{}",
                    self.transcript()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub(super) fn send(&mut self, text: &str) -> Result<()> {
        let mut input = self.input.lock().expect("terminal input lock");
        input.write_all(text.as_bytes())?;
        input.flush()?;
        Ok(())
    }

    pub(super) async fn answer(&mut self, question: &str, answer: &str) -> Result<()> {
        self.expect(question, PROMPT).await?;
        self.send(&format!("{answer}\r"))
    }

    pub(super) async fn answer_inference(&mut self, endpoint: &str, model: &str) -> Result<()> {
        self.answer(INFERENCE_QUESTION, "3").await?;
        self.answer("Backend base URL", endpoint).await?;
        self.answer("Model name:", model).await
    }

    /// The questions after the folder: a custom backend, and the default tools.
    pub(super) async fn answer_inference_and_tools(
        &mut self,
        endpoint: &str,
        model: &str,
    ) -> Result<()> {
        self.answer_inference(endpoint, model).await?;
        self.answer(TOOLS_QUESTION, "").await
    }

    /// Returns once the child exited and its output reached the transcript.
    pub(super) async fn wait_exit(&mut self, timeout: Duration) -> Result<u32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                while !self.drained.load(Ordering::SeqCst) && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                return Ok(status.exit_code());
            }
            if Instant::now() >= deadline {
                bail!(
                    "the child did not exit within {timeout:?}; transcript:\n{}",
                    self.transcript()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Returns once the interface has stopped drawing for a moment, or after `PROMPT`
    /// if it never does. The outcome a caller asserts is what counts.
    pub(super) async fn settle(&self) {
        let deadline = Instant::now() + PROMPT;
        let mut seen = self.transcript.lock().expect("transcript lock").len();
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = self.transcript.lock().expect("transcript lock").len();
            if now > 0 && now == seen {
                return;
            }
            seen = now;
        }
    }

    pub(super) async fn expect_exit(&mut self, code: u32) -> Result<()> {
        let actual = self.wait_exit(EXIT).await?;
        ensure!(
            actual == code,
            "expected exit {code}, got {actual}; transcript:\n{}",
            self.transcript()
        );
        Ok(())
    }
}

impl Drop for Terminal {
    /// The child leads its PTY session, so its group also holds the interface `gents`
    /// started. Real Grok still running when its runtime stops starts a detached stock
    /// leader on the socket, so the interface must be gone before `LiveBox` stops it.
    fn drop(&mut self) {
        // A reaped child's group may no longer be ours to signal. Until it is reaped, its
        // PID and so its group stay reserved.
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        if let Some(group) = self.child.process_id() {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .output();
        }
    }
}

/// The configured model server, checked before anything is installed.
pub(super) async fn live_model() -> Result<(String, String)> {
    let endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .context("with_model tests read GENTS_CLI_E2E_MODEL_ENDPOINT")?;
    let model = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .context("with_model tests read GENTS_CLI_E2E_MODEL_NAME")?;
    let models = format!("{}/models", endpoint.trim_end_matches('/'));
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(&models)
        .send()
        .await
        .with_context(|| format!("the model server at {models} is unreachable"))?
        .error_for_status()
        .with_context(|| format!("the model server at {models} refused the request"))?;
    Ok((endpoint, model))
}

/// The installed interface, or `None` after printing that the test is skipped.
pub(super) fn real_interface(name: &str) -> Option<PathBuf> {
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file());
    if found.is_none() {
        eprintln!("skipped: {name} is not installed");
    }
    found
}

pub(super) fn node_did_of(home: &Path) -> Result<String> {
    read_json_file(&home.join("init.json"))?["node_did"]
        .as_str()
        .map(str::to_string)
        .context("init.json names a node DID")
}

/// A new connection each call, as a client that just started would make.
pub(super) async fn shim_call(route: &str, method: &str, params: Value) -> Result<Value> {
    let url = format!("ws://127.0.0.1:{SHIM_PORT}{route}");
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .with_context(|| format!("connecting to {url}"))?;
    for message in [
        json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "cli-onboarding-live", "title": null, "version": "0"},
                "capabilities": null,
            },
        }),
        json!({"method": "initialized"}),
        json!({"id": 2, "method": method, "params": params}),
    ] {
        socket
            .send(Message::Text(message.to_string().into()))
            .await?;
    }
    while let Some(message) = socket.next().await {
        let Message::Text(text) = message? else {
            continue;
        };
        let reply: Value = serde_json::from_str(&text)?;
        if reply["id"] == json!(2) {
            ensure!(reply.get("error").is_none(), "{method} failed: {reply}");
            return Ok(reply["result"].clone());
        }
    }
    bail!("the shim closed before answering {method}")
}

pub(super) fn agent_route(agent: &str) -> String {
    format!("/agents/{agent}")
}

pub(super) async fn loaded_threads(route: &str) -> Result<Vec<String>> {
    Ok(serde_json::from_value(
        shim_call(route, "thread/loaded/list", json!({})).await?["data"].clone(),
    )?)
}

/// Waits for `route` to load a thread outside `known`. Nothing is pressed, so any Codex
/// screen in the way of a thread, such as its trust question, fails this.
pub(super) async fn wait_for_loaded_thread(
    terminal: &Terminal,
    route: &str,
    known: &[String],
) -> Result<String> {
    let deadline = Instant::now() + OPENS_THREAD;
    loop {
        if let Ok(loaded) = loaded_threads(route).await {
            if let Some(thread) = loaded.into_iter().find(|thread| !known.contains(thread)) {
                return Ok(thread);
            }
        }
        ensure!(
            Instant::now() < deadline,
            "Codex loaded no thread on {route} within {OPENS_THREAD:?}; transcript:\n{}",
            terminal.transcript()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Holds the shim port for a test, as something that is not a Gents runtime would.
pub(super) struct HeldShimPort(tokio::task::JoinHandle<()>);

impl HeldShimPort {
    /// With an empty `reply`, each connection is closed as soon as it arrives.
    pub(super) async fn serving(reply: &'static [u8]) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", SHIM_PORT)).await?;
        Ok(Self(tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                if reply.is_empty() {
                    continue;
                }
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(reply).await;
                let _ = stream.shutdown().await;
            }
        })))
    }

    pub(super) async fn release(self) {
        self.0.abort();
        let _ = self.0.await;
    }
}
