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
}

fn job_loaded(target: &str) -> Result<bool> {
    Ok(Command::new("/bin/launchctl")
        .args(["print", target])
        .output()
        .context("running launchctl print")?
        .status
        .success())
}

impl LiveBox {
    /// The label check is the safety boundary: this binary built without
    /// `native-e2e` addresses the user's daily service, so nothing may reach
    /// launchd until the build has reported its own label.
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
        fs::write(
            codex_home.join("config.toml"),
            "# Existing user Codex config should remain client-side.\n",
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
        })
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
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
    }
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
}
