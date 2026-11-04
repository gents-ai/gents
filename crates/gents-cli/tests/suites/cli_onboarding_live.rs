//! Bare `gents` in a real PTY against real launchd, with a temporary `HOME` and
//! no copied credentials. The label and ports are fixed, so tests run one at a
//! time, through `make live-onboarding`.
#![allow(dead_code)]

use crate::support::*;

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::Message;

const HTTP_PORT: u16 = 21919;
const SHIM_PORT: u16 = 21929;
const NO_MODEL_ENDPOINT: &str = "http://127.0.0.1:9/v1";
const NO_MODEL_NAME: &str = "no-model";
const MISSING_BIN: &str = "/nonexistent/gents-live-interface";
const INSTALL_MESSAGE: &str = "Codex was not found";
/// Longer than the 60 s `gents` itself waits for the endpoint.
const RUNTIME_UP: Duration = Duration::from_secs(90);
const EXIT: Duration = Duration::from_secs(10);
const PROMPT: Duration = Duration::from_secs(30);
const TURN: Duration = Duration::from_secs(240);
const FOLDER_QUESTION: &str = "Data folder [";
const INFERENCE_QUESTION: &str = "How do you want to run inference?";
const TOOLS_QUESTION: &str = "What may agents do?";
const ENDPOINT_TEXT: &str = "127.0.0.1:21929";
const OPENS_THREAD: Duration = Duration::from_secs(60);
const OUTSIDE_THE_ROOT_TEXT: &str = "the thread will use";

/// One test's temporary user. Owns the launchd job from the moment it exists,
/// and every refusal happens before that, so `Drop` never removes a job this
/// run did not load.
struct LiveBox {
    _temp: tempfile::TempDir,
    root: PathBuf,
    label: String,
    domain: String,
    homes: Mutex<Vec<PathBuf>>,
}

struct InitializedHome {
    home: PathBuf,
    node_did: String,
    default_agent_id: String,
    profile_id: String,
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
fn isolated_label(metadata: &Value) -> Result<&str> {
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
    fn new() -> Result<Self> {
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

    fn target(&self) -> String {
        format!("{}/{}", self.domain, self.label)
    }

    fn gents(&self) -> Command {
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
    fn folder(&self, name: &str) -> PathBuf {
        let folder = self.root.join(name);
        self.homes.lock().expect("homes lock").push(folder.clone());
        folder
    }

    fn log(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.terminal.log"))
    }

    fn remembered_home(&self) -> PathBuf {
        self.root.join("Library/Application Support/gents/home")
    }

    /// Bare `gents` choosing file store-key custody, as it would on a first run.
    fn first_run(&self, interface: &str) -> Command {
        let mut command = self.gents();
        command
            .env("GENTS_CODEX_BIN", interface)
            .args(["--store-key-custody", "file"]);
        command
    }

    fn init_home(&self, name: &str, endpoint: &str, model: &str) -> Result<InitializedHome> {
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
    async fn clone_agent(&self, home: &InitializedHome, display_name: &str) -> Result<String> {
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
    fn bare_gents(&self, interface: &str, home: &Path) -> Command {
        let mut command = self.gents();
        command
            .env("GENTS_CODEX_BIN", interface)
            .arg("--home")
            .arg(home);
        command
    }

    /// `gents --home <home>`, opening `interface` as its Grok binary.
    fn bare_gents_grok(&self, interface: &str, home: &Path) -> Command {
        let mut command = self.gents();
        command
            .env("GENTS_GROK_BIN", interface)
            .arg("--home")
            .arg(home);
        command
    }

    /// Brings up `home`'s service through bare `gents`, which then stops at the
    /// missing interface.
    async fn bring_up(&self, home: &Path) -> Result<()> {
        let mut terminal =
            Terminal::spawn(&self.bare_gents(MISSING_BIN, home), &self.log("bring-up"))?;
        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect_exit(1).await
    }

    fn gents_ok(&self, args: &[&str]) -> Result<()> {
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

    fn service_pid(&self) -> Option<u32> {
        let output = Command::new("/bin/launchctl")
            .args(["print", &self.target()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = ")?.parse().ok())
    }

    fn plist(&self) -> PathBuf {
        self.root
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", self.label))
    }

    fn plist_mtime(&self) -> Result<SystemTime> {
        Ok(fs::metadata(self.plist())?.modified()?)
    }

    fn label_disabled(&self) -> Result<bool> {
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
struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    transcript: Arc<Mutex<Vec<u8>>>,
    drained: Arc<AtomicBool>,
    cursor: usize,
    _master: Box<dyn MasterPty + Send>,
}

impl Terminal {
    fn spawn(command: &Command, log: &Path) -> Result<Self> {
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

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.transcript.lock().expect("transcript lock")).into_owned()
    }

    /// Waits for `needle` after the previous match, so a run of calls asserts order.
    async fn expect(&mut self, needle: &str, timeout: Duration) -> Result<()> {
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

    fn send(&mut self, text: &str) -> Result<()> {
        let mut input = self.input.lock().expect("terminal input lock");
        input.write_all(text.as_bytes())?;
        input.flush()?;
        Ok(())
    }

    async fn answer(&mut self, question: &str, answer: &str) -> Result<()> {
        self.expect(question, PROMPT).await?;
        self.send(&format!("{answer}\r"))
    }

    /// The questions after the folder: a custom backend, and the default tools.
    async fn answer_inference_and_tools(&mut self, endpoint: &str, model: &str) -> Result<()> {
        self.answer(INFERENCE_QUESTION, "3").await?;
        self.answer("Backend base URL", endpoint).await?;
        self.answer("Model name:", model).await?;
        self.answer(TOOLS_QUESTION, "").await
    }

    /// Returns once the child exited and its output reached the transcript.
    async fn wait_exit(&mut self, timeout: Duration) -> Result<u32> {
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
    async fn settle(&self) {
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

    async fn expect_exit(&mut self, code: u32) -> Result<()> {
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
        if let Some(group) = self.child.process_id() {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .output();
        }
    }
}

/// The configured model server, checked before anything is installed.
async fn live_model() -> Result<(String, String)> {
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
fn real_interface(name: &str) -> Option<PathBuf> {
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file());
    if found.is_none() {
        eprintln!("skipped: {name} is not installed");
    }
    found
}

fn node_did_of(home: &Path) -> Result<String> {
    read_json_file(&home.join("init.json"))?["node_did"]
        .as_str()
        .map(str::to_string)
        .context("init.json names a node DID")
}

/// A new connection each call, as a client that just started would make.
async fn shim_call(route: &str, method: &str, params: Value) -> Result<Value> {
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

fn agent_route(agent: &str) -> String {
    format!("/agents/{agent}")
}

async fn loaded_threads(route: &str) -> Result<Vec<String>> {
    Ok(serde_json::from_value(
        shim_call(route, "thread/loaded/list", json!({})).await?["data"].clone(),
    )?)
}

/// Waits for `route` to load a thread outside `known`. A `--cd` makes real Codex ask
/// whether to trust the directory, and it asks every time because the shim keeps no
/// answer. Enter accepts that, and does nothing in an empty composer.
async fn wait_for_loaded_thread(
    terminal: &mut Terminal,
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
        terminal.send("\r")?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Holds the shim port for a test, as something that is not a Gents runtime would.
struct HeldShimPort(tokio::task::JoinHandle<()>);

impl HeldShimPort {
    /// With an empty `reply`, each connection is closed as soon as it arrives.
    async fn serving(reply: &'static [u8]) -> Result<Self> {
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

    async fn release(self) {
        self.0.abort();
        let _ = self.0.await;
    }
}

mod no_model {
    use super::*;

    /// A home whose backend is down publishes no runnable agent, so `--agent` needs
    /// one that answers. It runs no turn here and needs no model server.
    fn answering_backend() -> Result<MockChatEndpoint> {
        MockChatEndpoint::start(NO_MODEL_NAME, "unused")
    }

    /// A stand-in interface that writes its arguments to `args` and exits 0.
    fn recording_interface(live: &LiveBox) -> Result<(PathBuf, PathBuf)> {
        use std::os::unix::fs::PermissionsExt as _;
        let script = live.root.join("recording-interface");
        let args = live.root.join("recorded-args");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", args.display()),
        )?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        Ok((script, args))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn codex_opens_in_the_terminal_directory_only_inside_the_tool_root() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        let (interface, recorded) = recording_interface(&live)?;
        let project = live.root.join("project");
        fs::create_dir(&project)?;
        let open = |directory: &Path, extra: &[&str], log: &str| {
            let mut command = live.bare_gents(&interface.to_string_lossy(), &home.home);
            command.current_dir(directory).args(extra);
            Terminal::spawn(&command, &live.log(log))
        };

        let mut terminal = open(&project, &[], "inside")?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let args = fs::read_to_string(&recorded)?;
        ensure!(
            args.contains(&format!("--cd\n{}\n", project.display())),
            "{args}"
        );
        ensure!(
            !terminal.transcript().contains(OUTSIDE_THE_ROOT_TEXT),
            "{}",
            terminal.transcript()
        );

        let mut terminal = open(Path::new("/"), &[], "outside")?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let args = fs::read_to_string(&recorded)?;
        ensure!(!args.contains("--cd"), "{args}");
        ensure!(
            terminal
                .transcript()
                .contains(&format!("{OUTSIDE_THE_ROOT_TEXT} {}", live.root.display())),
            "{}",
            terminal.transcript()
        );

        let mut terminal = open(&project, &["--agent", &home.default_agent_id], "agent")?;
        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        let args = fs::read_to_string(&recorded)?;
        ensure!(
            args.contains(&format!("--cd\n{}\n", project.display())),
            "{args}"
        );
        ensure!(
            !args.contains(&format!("127.0.0.1:{SHIM_PORT}")),
            "--agent must connect through the relay: {args}"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn the_first_open_after_a_start_waits_for_the_publish() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        live.gents_ok(&["service", "stop", "--home", &home.home.to_string_lossy()])?;

        let mut command = live.bare_gents("/usr/bin/true", &home.home);
        command.args(["--agent", &home.default_agent_id]);
        let mut terminal = Terminal::spawn(&command, &live.log("first-open"))?;

        let code = terminal.wait_exit(RUNTIME_UP).await?;
        ensure!(code == 0, "{}", terminal.transcript());
        ensure!(
            !terminal.transcript().contains("unknown agent"),
            "{}",
            terminal.transcript()
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_unknown_agent_names_gents_config_agent_list() -> Result<()> {
        let backend = answering_backend()?;
        let live = LiveBox::new()?;
        let home = live.init_home("home", backend.endpoint(), NO_MODEL_NAME)?;
        let mut command = live.bare_gents("/usr/bin/true", &home.home);
        command.args(["--agent", "nosuch"]);

        let mut terminal = Terminal::spawn(&command, &live.log("unknown"))?;

        terminal.expect("unknown agent", RUNTIME_UP).await?;
        terminal.expect("gents config agent list", EXIT).await?;
        terminal.expect_exit(1).await
    }

    #[test]
    #[ignore = "ignored only so `make live-onboarding` runs it; touches nothing"]
    fn only_an_isolated_build_on_macos_passes_the_label_check() {
        for (os, label) in [
            ("macos", "ai.gents.runtime"),
            ("macos", ""),
            ("macos", "ai.gents.runtime.native-e2e.other"),
            ("linux", "ai.gents.runtime.native-e2e"),
        ] {
            let metadata = json!({"os": os, "service_label": label});
            assert!(
                isolated_label(&metadata).is_err(),
                "{os} {label} must be refused"
            );
        }
        assert!(isolated_label(&json!({})).is_err());
        assert_eq!(
            isolated_label(&json!({"os": "macos", "service_label": "ai.gents.runtime.native-e2e"}))
                .unwrap(),
            "ai.gents.runtime.native-e2e"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_stopped_home_starts_its_service_and_reports_the_missing_interface() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;

        let mut terminal = Terminal::spawn(
            &live.bare_gents(MISSING_BIN, &home.home),
            &live.log("first-open"),
        )?;

        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect("gents chat", EXIT).await?;
        terminal.expect_exit(1).await?;
        ensure!(live.plist().is_file(), "the service was not installed");
        ensure!(
            live.label_disabled()?,
            "a first open must not enable the service at login"
        );
        shim_call("/", "thread/loaded/list", json!({})).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_running_runtime_is_left_alone() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        let pid = live.service_pid().context("the service runs")?;
        let mtime = live.plist_mtime()?;

        let mut terminal = Terminal::spawn(
            &live.bare_gents("/usr/bin/false", &home.home),
            &live.log("second-open"),
        )?;

        terminal.expect("Codex exited", RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        assert_eq!(live.service_pid(), Some(pid));
        assert_eq!(live.plist_mtime()?, mtime);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_stopped_service_is_started_without_reinstalling() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        live.bring_up(&home.home).await?;
        let pid = live.service_pid().context("the service runs")?;
        let mtime = live.plist_mtime()?;
        live.gents_ok(&["service", "stop", "--home", &home.home.to_string_lossy()])?;

        live.bring_up(&home.home).await?;

        let restarted = live.service_pid().context("the service runs again")?;
        assert_ne!(restarted, pid);
        assert_eq!(live.plist_mtime()?, mtime);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn another_homes_runtime_is_refused_and_named() -> Result<()> {
        let live = LiveBox::new()?;
        let a = live.init_home("home-a", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let b = live.init_home("home-b", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        live.bring_up(&a.home).await?;
        let pid = live.service_pid().context("home A's service runs")?;

        let mut terminal =
            Terminal::spawn(&live.bare_gents(MISSING_BIN, &b.home), &live.log("refused"))?;

        terminal
            .expect(&a.home.to_string_lossy(), RUNTIME_UP)
            .await?;
        terminal.expect_exit(1).await?;
        ensure!(
            terminal.transcript().contains("gents --home"),
            "the refusal must say how to open the other home: {}",
            terminal.transcript()
        );
        assert_eq!(live.service_pid(), Some(pid));

        live.gents_ok(&["service", "stop", "--home", &a.home.to_string_lossy()])?;
        let mut terminal = Terminal::spawn(
            &live.bare_gents(MISSING_BIN, &b.home),
            &live.log("owned-by-a"),
        )?;

        terminal.expect("belongs to Gents home", RUNTIME_UP).await?;
        terminal.expect_exit(1).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_endpoint_that_cannot_identify_itself_gets_the_same_refusal() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let endpoint = format!("127.0.0.1:{SHIM_PORT}");

        for (name, reply) in [
            ("closes", &b""[..]),
            (
                "not-found",
                &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..],
            ),
        ] {
            let held = HeldShimPort::serving(reply).await?;
            let mut terminal =
                Terminal::spawn(&live.bare_gents(MISSING_BIN, &home.home), &live.log(name))?;

            terminal.expect(&endpoint, RUNTIME_UP).await?;
            terminal.expect_exit(1).await?;
            ensure!(!live.plist().exists(), "{name}: nothing may be installed");
            held.release().await;
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_runtime_up_with_its_endpoint_off_says_restart_and_restarts_nothing() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("home", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let child = live
            .gents()
            .args(["server", "--no-codex-shim", "--home"])
            .arg(&home.home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting the foreground server")?;
        let mut server = ServeProcess::new(child);
        let graphql = graphql_url(HTTP_PORT);
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        wait_for_runtime_state_graphql(&home.home, &graphql, RUNTIME_UP).await?;

        let mut terminal = Terminal::spawn(
            &live.bare_gents(MISSING_BIN, &home.home),
            &live.log("endpoint-off"),
        )?;

        terminal.expect("gents service restart", RUNTIME_UP).await?;
        terminal.expect("whatever is running it", EXIT).await?;
        terminal.expect_exit(1).await?;
        ensure!(
            server.child.try_wait()?.is_none(),
            "the foreground server must still be running"
        );
        ensure!(!live.plist().exists(), "nothing may be installed");
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn first_run_sets_up_and_leaves_the_runtime_up() -> Result<()> {
        let live = LiveBox::new()?;
        let folder = live.folder("my data");

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("first-run"))?;

        for row in [
            "      ██████  ",
            "██████        ",
            "          ████",
            "    ██        ",
            "      ██████  ",
        ] {
            terminal.expect(row, PROMPT).await?;
        }
        terminal.expect(INSTALL_MESSAGE, PROMPT).await?;
        terminal
            .answer(FOLDER_QUESTION, &folder.to_string_lossy())
            .await?;
        terminal
            .answer_inference_and_tools(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect("gents chat", EXIT).await?;
        terminal.expect_exit(1).await?;

        assert_eq!(
            read_json_file(&folder.join("init.json"))?["tool_ceiling"],
            json!("Readonly")
        );
        assert_eq!(
            fs::read_to_string(live.remembered_home())?.trim_end_matches('\n'),
            folder.to_string_lossy()
        );
        ensure!(
            !terminal.transcript().contains(r#""status":"initialized""#)
                && !terminal.transcript().contains(r#""status": "initialized""#),
            "setup must not print init's JSON: {}",
            terminal.transcript()
        );
        ensure!(live.plist().is_file(), "the service was not installed");
        shim_call("/", "thread/loaded/list", json!({})).await?;

        let pid = live.service_pid().context("the service runs")?;
        let mut no_flags = live.gents();
        no_flags.env("GENTS_CODEX_BIN", MISSING_BIN);
        let mut second = Terminal::spawn(&no_flags, &live.log("second-run"))?;
        second.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        second.expect_exit(1).await?;
        ensure!(
            !second.transcript().contains(FOLDER_QUESTION)
                && !second.transcript().contains(INFERENCE_QUESTION),
            "a set up home asks nothing: {}",
            second.transcript()
        );
        assert_eq!(live.service_pid(), Some(pid));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_failed_setup_remembers_nothing() -> Result<()> {
        let live = LiveBox::new()?;
        let file = live.root.join("a-file");
        fs::write(&file, "not a folder")?;

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("failed"))?;
        terminal
            .answer(FOLDER_QUESTION, &file.to_string_lossy())
            .await?;
        let code = terminal.wait_exit(PROMPT).await?;

        ensure!(code != 0, "{}", terminal.transcript());
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        ensure!(!live.plist().exists(), "nothing is installed");
        assert_eq!(fs::read_to_string(&file)?, "not a folder");

        let mut rerun = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("rerun"))?;
        rerun.expect(FOLDER_QUESTION, PROMPT).await?;
        rerun.send("\x03")?;
        rerun.wait_exit(EXIT).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn an_initialized_folder_skips_the_remaining_questions() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.init_home("existing", NO_MODEL_ENDPOINT, NO_MODEL_NAME)?;
        let held = HeldShimPort::serving(b"").await?;

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("existing"))?;
        terminal
            .answer(FOLDER_QUESTION, &home.home.to_string_lossy())
            .await?;
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

        ensure!(
            !terminal.transcript().contains(INFERENCE_QUESTION)
                && !terminal.transcript().contains(TOOLS_QUESTION),
            "an initialized folder asks nothing more: {}",
            terminal.transcript()
        );
        assert_eq!(
            fs::read_to_string(live.remembered_home())?.trim_end_matches('\n'),
            home.home.to_string_lossy()
        );
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_given_home_is_not_asked_and_not_remembered() -> Result<()> {
        let live = LiveBox::new()?;
        let home = live.folder("given");
        let held = HeldShimPort::serving(b"").await?;
        let mut command = live.first_run(MISSING_BIN);
        command.arg("--home").arg(&home);

        let mut terminal = Terminal::spawn(&command, &live.log("given"))?;
        terminal
            .answer_inference_and_tools(NO_MODEL_ENDPOINT, NO_MODEL_NAME)
            .await?;
        terminal.expect(ENDPOINT_TEXT, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;

        ensure!(
            !terminal.transcript().contains(FOLDER_QUESTION),
            "a given home is not asked for: {}",
            terminal.transcript()
        );
        ensure!(home.join("init.json").is_file(), "the given home is set up");
        ensure!(!live.remembered_home().exists(), "nothing is remembered");
        held.release().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "drives real launchd and a PTY; run through `make live-onboarding`"]
    async fn a_vanished_remembered_folder_is_the_suggested_answer() -> Result<()> {
        let live = LiveBox::new()?;
        let vanished = live.root.join("vanished folder");
        let remembered = live.remembered_home();
        fs::create_dir_all(remembered.parent().context("the file has a parent")?)?;
        fs::write(&remembered, format!("{}\n", vanished.display()))?;

        let mut terminal = Terminal::spawn(&live.first_run(MISSING_BIN), &live.log("vanished"))?;
        terminal
            .expect(&format!("{FOLDER_QUESTION}{}]", vanished.display()), PROMPT)
            .await?;
        terminal.send("\x03")?;
        terminal.wait_exit(EXIT).await?;
        Ok(())
    }
}

mod with_model {
    use super::*;

    fn unique_prompt() -> String {
        format!("Reply with exactly this token: {}", uuid::Uuid::new_v4())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn two_real_codex_windows_keep_agents_apart() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let codex = codex.to_string_lossy();
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        let pid = live.service_pid().context("the service runs")?;
        let a = home.default_agent_id.clone();
        let b = live.clone_agent(&home, "other").await?;
        let (route_a, route_b) = (agent_route(&a), agent_route(&b));
        let (prompt_a, prompt_b) = (unique_prompt(), unique_prompt());

        let window = |agent: &str, prompt: &str, log: &str| {
            let mut command = live.gents();
            command.env("GENTS_CODEX_BIN", &*codex).args([
                "codex",
                "--agent",
                agent,
                "--no-alt-screen",
                prompt,
            ]);
            Terminal::spawn(&command, &live.log(log))
        };
        let mut window_a = window(&a, &prompt_a, "window-a")?;
        let mut window_b = window(&b, &prompt_b, "window-b")?;

        wait_for_loaded_thread(&mut window_a, &route_a, &[]).await?;
        wait_for_loaded_thread(&mut window_b, &route_b, &[]).await?;
        let graphql = graphql_url(HTTP_PORT);
        let turns: Result<_> = async {
            let (request_a, session_a, agent_a) =
                wait_for_request(&graphql, &home.node_did, &prompt_a).await?;
            let (request_b, session_b, agent_b) =
                wait_for_request(&graphql, &home.node_did, &prompt_b).await?;
            assert_eq!(agent_a, a);
            assert_eq!(agent_b, b);
            assert_ne!(session_a, session_b);
            wait_for_request_lifecycle_state(&graphql, &request_a, &["completed"], TURN).await?;
            wait_for_request_lifecycle_state(&graphql, &request_b, &["completed"], TURN).await?;
            Ok((session_a, session_b))
        }
        .await;
        let (session_a, session_b) = turns.with_context(|| {
            format!(
                "window A:\n{}\nwindow B:\n{}",
                window_a.transcript(),
                window_b.transcript()
            )
        })?;

        for (route, own, other) in [
            (&route_a, &session_a, &session_b),
            (&route_b, &session_b, &session_a),
        ] {
            let listed = shim_call(route, "thread/list", json!({})).await?;
            let listed = listed["data"].as_array().context("thread/list has data")?;
            let has = |id: &String| listed.iter().any(|thread| &thread["id"] == id);
            ensure!(has(own) && !has(other), "{route} lists {listed:?}");
            let loaded = loaded_threads(route).await?;
            ensure!(
                loaded.contains(own) && !loaded.contains(other),
                "{route} loads {loaded:?}"
            );
        }

        let project = live.root.join("project");
        fs::create_dir(&project)?;
        let known = loaded_threads(&route_b).await?;
        let mut from_project = live.bare_gents(&codex, &home.home);
        from_project.current_dir(&project).args(["--agent", &b]);
        let mut window_project = Terminal::spawn(&from_project, &live.log("window-project"))?;
        let thread = wait_for_loaded_thread(&mut window_project, &route_b, &known).await?;
        let listed = shim_call(&route_b, "thread/list", json!({})).await?;
        let started = listed["data"]
            .as_array()
            .and_then(|threads| {
                threads
                    .iter()
                    .find(|listed| listed["id"] == thread.as_str())
            })
            .with_context(|| format!("{thread} is not listed: {listed}"))?;
        assert_eq!(started["cwd"], json!(project.to_string_lossy()));

        assert_eq!(live.service_pid(), Some(pid));
        Ok(())
    }

    /// Opens Grok through `command`, submits a prompt, and returns the request's agent
    /// once it completes.
    async fn turn_in_grok(
        live: &LiveBox,
        home: &InitializedHome,
        command: &Command,
        log: &str,
    ) -> Result<String> {
        let mut terminal = Terminal::spawn(command, &live.log(log))?;
        terminal.settle().await;
        let prompt = unique_prompt();
        terminal.send(&prompt)?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        terminal.send("\r")?;

        let graphql = graphql_url(HTTP_PORT);
        let result: Result<String> = async {
            let (request, _, agent) = wait_for_request(&graphql, &home.node_did, &prompt).await?;
            wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
            Ok(agent)
        }
        .await;
        result.with_context(|| format!("Grok transcript:\n{}", terminal.transcript()))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Grok; run through `make live-onboarding`"]
    async fn real_grok_opens_by_setting_and_by_agent() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(grok) = real_interface("grok") else {
            return Ok(());
        };
        let grok = grok.to_string_lossy();
        let socket = Path::new("/tmp/gents-grok.sock");
        ensure!(
            !socket.exists(),
            "{} exists, so another runtime holds Grok's socket",
            socket.display()
        );
        let live = LiveBox::new()?;
        let home = live.init_home("home", &endpoint, &model)?;
        let home_arg = home.home.to_string_lossy();
        live.bring_up(&home.home).await?;
        crate::support::graphql::register_served_home(&home.home, HTTP_PORT)?;
        let b = live.clone_agent(&home, "other").await?;
        let pid = live.service_pid().context("the service runs")?;

        live.gents_ok(&["config", "client", "set", "grok", "--home", &home_arg])?;
        let mut refused = Terminal::spawn(
            &live.bare_gents_grok(&grok, &home.home),
            &live.log("grok-socket-off"),
        )?;
        refused.expect("gents service restart", RUNTIME_UP).await?;
        refused.expect_exit(1).await?;
        assert_eq!(live.service_pid(), Some(pid));

        live.gents_ok(&["service", "restart", "--home", &home_arg])?;
        let agent = turn_in_grok(
            &live,
            &home,
            &live.bare_gents_grok(&grok, &home.home),
            "grok-default",
        )
        .await?;
        assert_eq!(agent, home.default_agent_id);

        let mut by_agent = live.bare_gents_grok(&grok, &home.home);
        by_agent.args(["--agent", &b]);
        let agent = turn_in_grok(&live, &home, &by_agent, "grok-agent").await?;
        assert_eq!(agent, b);

        drop(live);
        ensure!(
            !socket.exists(),
            "{} outlived the runtime; `lsof` names what holds it",
            socket.display()
        );
        Ok(())
    }

    /// Runs first-run setup into `folder` against the model, up to opening the interface.
    async fn set_up(
        live: &LiveBox,
        interface: &str,
        folder: &Path,
        endpoint: &str,
        model: &str,
    ) -> Result<Terminal> {
        let mut terminal = Terminal::spawn(&live.first_run(interface), &live.log("first-run"))?;
        terminal
            .answer(FOLDER_QUESTION, &folder.to_string_lossy())
            .await?;
        terminal.answer_inference_and_tools(endpoint, model).await?;
        Ok(terminal)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model and real launchd; run through `make live-onboarding`"]
    async fn first_run_then_chat_completes_a_turn() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let live = LiveBox::new()?;
        let folder = live.folder("data");

        let mut terminal = set_up(&live, MISSING_BIN, &folder, &endpoint, &model).await?;
        terminal.expect(INSTALL_MESSAGE, RUNTIME_UP).await?;
        terminal.expect_exit(1).await?;
        crate::support::graphql::register_served_home(&folder, HTTP_PORT)?;

        let prompt = format!("Reply with exactly this token: {}", uuid::Uuid::new_v4());
        let mut chat = live.gents();
        chat.args(["chat", "--home"])
            .arg(&folder)
            .args(["--output", "json", &prompt]);
        let output = tokio::time::timeout(
            TURN,
            tokio::process::Command::from(chat)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("gents chat did not finish")??;
        ensure!(
            output.status.success(),
            "gents chat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let graphql = graphql_url(HTTP_PORT);
        let (request, _, _) = wait_for_request(&graphql, &node_did_of(&folder)?, &prompt).await?;
        wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs a local model, real launchd and real Codex; run through `make live-onboarding`"]
    async fn first_run_opens_real_codex_and_a_turn_completes() -> Result<()> {
        let (endpoint, model) = live_model().await?;
        let Some(codex) = real_interface("codex") else {
            return Ok(());
        };
        let live = LiveBox::new()?;
        let folder = live.folder("data");
        let codex = codex.to_string_lossy();

        let mut terminal = set_up(&live, &codex, &folder, &endpoint, &model).await?;
        let result = turn_in_codex(&live, &mut terminal, &folder).await;
        result.with_context(|| format!("Codex transcript:\n{}", terminal.transcript()))
    }

    async fn turn_in_codex(live: &LiveBox, terminal: &mut Terminal, folder: &Path) -> Result<()> {
        wait_for_loaded_thread(terminal, "/", &[]).await?;
        let pid = live.service_pid().context("the service runs")?;
        crate::support::graphql::register_served_home(folder, HTTP_PORT)?;

        let prompt = format!("Reply with exactly this token: {}", uuid::Uuid::new_v4());
        terminal.send(&prompt)?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        terminal.send("\r")?;

        let graphql = graphql_url(HTTP_PORT);
        let (request, _, _) = wait_for_request(&graphql, &node_did_of(folder)?, &prompt).await?;
        wait_for_request_lifecycle_state(&graphql, &request, &["completed"], TURN).await?;

        terminal.send("\x03")?;
        terminal.wait_exit(PROMPT).await.with_context(|| {
            let events = folder.join("codex-ui/log/codex-shim-events.jsonl");
            format!(
                "what the shim last received:\n{}",
                fs::read_to_string(events).unwrap_or_default()
            )
        })?;
        assert_eq!(live.service_pid(), Some(pid));
        shim_call("/", "thread/loaded/list", json!({})).await?;
        Ok(())
    }
}
