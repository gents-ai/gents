use crate::support::*;

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, ensure, Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde_json::Value;

pub(super) const HTTP_PORT: u16 = 21919;
pub(super) const NO_MODEL_ENDPOINT: &str = "http://127.0.0.1:9/v1";
pub(super) const NO_MODEL_NAME: &str = "no-model";
/// The built-in shell turns on mouse capture once raw mode and the alternate screen are on.
pub(super) const SHELL_UP: &str = "\x1b[?1006h";
/// Longer than the 60 s `gents` itself waits for the endpoint.
pub(super) const RUNTIME_UP: Duration = Duration::from_secs(90);
pub(super) const EXIT: Duration = Duration::from_secs(10);
pub(super) const PROMPT: Duration = Duration::from_secs(30);
pub(super) const TURN: Duration = Duration::from_secs(240);
pub(super) const FOLDER_QUESTION: &str = "Data folder [";
pub(super) const INFERENCE_QUESTION: &str = "Choose an inference provider";
/// The answer to `INFERENCE_QUESTION` that picks a local server.
pub(super) const LOCAL_PROVIDER: &str = "4";
pub(super) const NO_MODEL_TEXT: &str = "No model answered at";
pub(super) const CLEAR_SCREEN: &str = "\x1b[2J";
pub(super) const PTY_ROWS: u16 = 40;
pub(super) const PTY_COLUMNS: u16 = 120;
pub(super) const ROOT_QUESTION: &str = "Which folder should agents work in?";
/// A relative answer to `ROOT_QUESTION`, because every test's terminal starts in its
/// temporary `HOME`. `LiveBox::new` creates it.
pub(super) const WORK_FOLDER: &str = "work";

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
    pub(super) default_agent_did: String,
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
            .context("running gents runtime service update-check")?;
        ensure!(
            output.status.success(),
            "gents runtime service update-check failed: {}",
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
        ensure!(
            std::net::TcpListener::bind(("127.0.0.1", HTTP_PORT)).is_ok(),
            "port {HTTP_PORT} is in use; if a killed run left it, run `launchctl bootout {target}`"
        );

        fs::create_dir(root.join(WORK_FOLDER))?;
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
            .env("TERM", "xterm-256color")
            .env("RUST_LOG", "error")
            .env_remove("GENTS_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_RUNTIME_DIR")
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
    pub(super) fn first_run(&self) -> Command {
        let mut command = self.gents();
        command.args(["--store-key-custody", "file"]);
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
            default_agent_did: init["default_agent_did"]
                .as_str()
                .context("init reports a default agent")?
                .to_string(),
        })
    }

    /// `gents --home <home>`.
    pub(super) fn bare_gents(&self, home: &Path) -> Command {
        let mut command = self.gents();
        command.arg("--home").arg(home);
        command
    }

    /// Brings up `home`'s service through bare `gents`, then quits the shell it opens.
    pub(super) async fn bring_up(&self, home: &Path) -> Result<()> {
        let mut terminal = Terminal::spawn(&self.bare_gents(home), &self.log("bring-up"))?;
        terminal.quit_shell().await
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
                .args(["runtime", "service", "uninstall", "--home"])
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

/// The five queries `write_expect_smoke` answers for a terminal UI's startup.
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
            rows: PTY_ROWS,
            cols: PTY_COLUMNS,
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
        self.answer(INFERENCE_QUESTION, LOCAL_PROVIDER).await?;
        self.answer("Server base URL", endpoint).await?;
        self.answer("Model", model).await
    }

    /// A local server that lists `model`, whose recommended values are all kept.
    pub(super) async fn answer_scanned_inference(
        &mut self,
        endpoint: &str,
        model: &str,
    ) -> Result<()> {
        self.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        self.answer(INFERENCE_QUESTION, LOCAL_PROVIDER).await?;
        self.answer("Server base URL", endpoint).await?;
        self.answer("part of a name", model).await?;
        self.answer("Temperature", "").await?;
        self.answer("Top-p", "").await
    }

    /// Every question `init` needs, for a setup that fails before it reaches the model.
    pub(super) async fn answer_root_and_server(&mut self, endpoint: &str) -> Result<()> {
        self.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        self.answer(INFERENCE_QUESTION, LOCAL_PROVIDER).await?;
        self.answer("Server base URL", endpoint).await
    }

    /// The questions after the data folder: where agents work, then a local backend.
    pub(super) async fn answer_root_and_inference(
        &mut self,
        endpoint: &str,
        model: &str,
    ) -> Result<()> {
        self.answer(ROOT_QUESTION, WORK_FOLDER).await?;
        self.answer_inference(endpoint, model).await
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

    /// Bare `gents` opens the built-in shell once setup finishes; `esc :q` quits it.
    pub(super) async fn quit_shell(&mut self) -> Result<()> {
        self.expect(SHELL_UP, RUNTIME_UP).await?;
        self.send("\x1b:q\r")?;
        self.expect_exit(0).await
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
    /// started.
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

pub(super) fn node_did_of(home: &Path) -> Result<String> {
    read_json_file(&home.join("init.json"))?["node_did"]
        .as_str()
        .map(str::to_string)
        .context("init.json names a node DID")
}
