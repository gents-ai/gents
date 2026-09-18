//! Per-user native service ownership for the foreground Gents runtime.
//!
//! This module owns only OS service registration and process state. A running
//! service is not proof that the runtime is ready, healthy, or using a
//! particular identity; callers must keep using the runtime status APIs for
//! those checks.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub const SERVICE_LABEL: &str = "ai.gents.runtime";
pub const SYSTEMD_UNIT: &str = "gents-runtime.service";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeServicePlatform {
    Macos,
    Linux,
}

impl NativeServicePlatform {
    pub fn current() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Self::Macos)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else {
            bail!("native Gents services are supported only on macOS and Linux")
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeServiceConfig {
    pub home: PathBuf,
    pub executable: PathBuf,
    pub user_home: PathBuf,
    /// Fully resolved service-manager configuration directory. Keeping this
    /// in the value makes injected managers independent of process-global env.
    pub service_config_dir: PathBuf,
    /// The one non-secret environment value intentionally forwarded to the
    /// service, so configured host tools remain discoverable after detaching
    /// from a terminal. No other caller environment is serialized.
    pub search_path: Option<OsString>,
}

impl NativeServiceConfig {
    pub fn new(home: PathBuf, executable: PathBuf) -> Result<Self> {
        let user_home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .context("HOME is required to manage a per-user native service")?;
        let home = normalize_absolute(&home)?;
        if !executable.is_absolute() {
            bail!("native service executable must be an absolute path");
        }
        // Keep the requested absolute path available for status/stop/uninstall
        // even if a packaged sidecar has since been removed. Install/start do
        // the existence and executable-bit checks.
        let executable = if executable.exists() {
            fs::canonicalize(&executable)
                .with_context(|| format!("resolving Gents executable {}", executable.display()))?
        } else {
            executable
        };
        let platform = NativeServicePlatform::current()?;
        let service_config_dir = match platform {
            NativeServicePlatform::Macos => user_home.join("Library/LaunchAgents"),
            NativeServicePlatform::Linux => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| user_home.join(".config"))
                .join("systemd/user"),
        };
        Ok(Self {
            home,
            executable,
            user_home,
            service_config_dir,
            search_path: std::env::var_os("PATH").filter(|value| !value.is_empty()),
        })
    }

    pub fn definition_path(&self, platform: NativeServicePlatform) -> PathBuf {
        match platform {
            NativeServicePlatform::Macos => self
                .service_config_dir
                .join(format!("{SERVICE_LABEL}.plist")),
            NativeServicePlatform::Linux => self.service_config_dir.join(SYSTEMD_UNIT),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeServiceStatus {
    pub installed: bool,
    pub running: bool,
    /// The native supervisor still owns an active or transitional job. On
    /// macOS this includes loaded jobs that exited and are awaiting respawn.
    pub job_loaded: bool,
    pub enabled: bool,
    pub detail: Option<String>,
}

impl NativeServiceStatus {
    pub fn is_active_or_transitioning(&self) -> bool {
        self.job_loaded
    }

    pub fn summary(&self) -> String {
        format!(
            "installed={} running={} job_loaded={} enabled={} (native service state only; runtime health is not checked)",
            self.installed, self.running, self.job_loaded, self.enabled
        )
    }
}

#[derive(Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput>;
}

pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        use wait_timeout::ChildExt as _;

        let mut stdout = tempfile::tempfile().context("creating native command stdout capture")?;
        let mut stderr = tempfile::tempfile().context("creating native command stderr capture")?;
        let mut command = Command::new(program);
        if program == OsStr::new("systemctl") {
            command.arg("--no-pager");
        }
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()
            .with_context(|| format!("executing {}", Path::new(program).display()))?;
        let timeout = command_timeout(program, args);
        let waited = match child.wait_timeout(timeout) {
            Ok(waited) => waited,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("waiting for native service command");
            }
        };
        let status = match waited {
            Some(status) => status,
            None => {
                let kill_error = child.kill().err();
                let wait_error = child.wait().err();
                bail!(
                    "native service command timed out after {} seconds{}{}",
                    timeout.as_secs(),
                    kill_error
                        .map(|error| format!("; kill failed: {error}"))
                        .unwrap_or_default(),
                    wait_error
                        .map(|error| format!("; reap failed: {error}"))
                        .unwrap_or_default()
                );
            }
        };
        fn read_capture(file: &mut fs::File) -> Result<String> {
            const MAX_OUTPUT: u64 = 64 * 1024;
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            file.take(MAX_OUTPUT).read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
        }
        Ok(CommandOutput {
            success: status.success(),
            stdout: read_capture(&mut stdout)?,
            stderr: read_capture(&mut stderr)?,
        })
    }
}

fn command_timeout(program: &OsStr, args: &[OsString]) -> Duration {
    const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
    const STOP_TIMEOUT: Duration = Duration::from_secs(120);

    let is_stop = (program == OsStr::new("systemctl")
        && args.iter().any(|arg| arg == OsStr::new("stop")))
        || (program == OsStr::new("launchctl")
            && args.first().is_some_and(|arg| arg == OsStr::new("bootout")));
    if is_stop {
        STOP_TIMEOUT
    } else {
        CONTROL_TIMEOUT
    }
}

pub struct NativeServiceManager<R = ProcessCommandRunner> {
    config: NativeServiceConfig,
    platform: NativeServicePlatform,
    runner: R,
}

impl NativeServiceManager<ProcessCommandRunner> {
    pub fn new(config: NativeServiceConfig) -> Result<Self> {
        Ok(Self {
            config,
            platform: NativeServicePlatform::current()?,
            runner: ProcessCommandRunner,
        })
    }
}

impl<R: CommandRunner> NativeServiceManager<R> {
    pub fn with_runner(
        config: NativeServiceConfig,
        platform: NativeServicePlatform,
        runner: R,
    ) -> Self {
        Self {
            config,
            platform,
            runner,
        }
    }

    pub fn install(&self) -> Result<()> {
        validate_config(&self.config)?;
        let path = self.config.definition_path(self.platform);
        let parent = path
            .parent()
            .context("native service definition has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("creating service directory {}", parent.display()))?;
        let contents = self.render_definition()?;
        if path.exists() {
            let installed = fs::read(&path)
                .with_context(|| format!("reading installed native service {}", path.display()))?;
            self.ensure_owned_definition(&installed)?;
            if installed != contents.as_bytes() {
                let status = self.status()?;
                if status.running
                    || (self.platform == NativeServicePlatform::Linux
                        && status.is_active_or_transitioning())
                {
                    bail!("native service is running or transitioning; stop it before updating its executable or environment");
                }
                if self.platform == NativeServicePlatform::Macos {
                    let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
                    let loaded =
                        self.run_allow_failure("launchctl", &["print".into(), target.into()])?;
                    ensure_launchd_print_result(&loaded)?;
                    if loaded.success {
                        self.stop(false)?;
                    }
                }
                atomic_write(&path, contents.as_bytes()).with_context(|| {
                    format!("updating native service definition {}", path.display())
                })?;
                if self.platform == NativeServicePlatform::Linux {
                    self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
                }
            }
            return Ok(());
        }
        // A launchd disabled override is valid before the plist exists. Record
        // it first so a failed disable cannot leave a RunAtLoad artifact that
        // starts unexpectedly at the next login.
        if self.platform == NativeServicePlatform::Macos {
            self.set_macos_enabled(false)?;
        }
        atomic_write(&path, contents.as_bytes())
            .with_context(|| format!("writing native service definition {}", path.display()))?;
        // Installation is intentionally non-starting and disabled. Enablement
        // remains OS-native state rather than a second Gents preference file.
        if self.platform == NativeServicePlatform::Linux {
            self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
        }
        if self.platform == NativeServicePlatform::Linux {
            self.set_enabled(false)?;
        }
        Ok(())
    }

    pub fn start(&self, enable_at_login: bool) -> Result<()> {
        self.require_installed()?;
        let was_enabled = self.status()?.enabled;
        if enable_at_login {
            self.set_enabled(true)?;
        }
        match self.platform {
            NativeServicePlatform::Macos => {
                // launchd refuses to bootstrap a disabled label. Temporarily
                // enable it for a one-shot start, then restore disabled state;
                // disabling an already loaded job does not stop that job.
                if !enable_at_login && !was_enabled {
                    self.set_enabled(true)?;
                }
                let start_result = (|| -> Result<()> {
                    let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
                    let domain = format!("gui/{}", self.current_uid()?);
                    let path = self.config.definition_path(self.platform);
                    let bootstrap = self.run_allow_failure(
                        "launchctl",
                        &["bootstrap".into(), domain.into(), path.into_os_string()],
                    )?;
                    // bootstrap fails when already loaded; kickstart remains
                    // the idempotent operation for the loaded job.
                    let kick = self.runner.run(
                        OsStr::new("launchctl"),
                        &["kickstart".into(), target.into()],
                    )?;
                    if !kick.success {
                        bail!(
                            "launchctl could not start Gents: {} (bootstrap: {})",
                            output_detail(&kick),
                            output_detail(&bootstrap)
                        );
                    }
                    Ok(())
                })();
                let restore_result = if !enable_at_login && !was_enabled {
                    self.set_enabled(false)
                } else {
                    Ok(())
                };
                match (start_result, restore_result) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(start), Ok(())) => Err(start),
                    (Ok(()), Err(restore)) => Err(restore).context(
                        "Gents started, but restoring disabled-at-login state failed; enablement is uncertain",
                    ),
                    (Err(start), Err(restore)) => bail!(
                        "Gents failed to start: {start:#}; restoring disabled-at-login state also failed, so enablement is uncertain: {restore:#}"
                    ),
                }
            }
            NativeServicePlatform::Linux => {
                self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
                self.run_checked(
                    "systemctl",
                    &["--user".into(), "start".into(), SYSTEMD_UNIT.into()],
                )
            }
        }
    }

    pub fn stop(&self, disable_at_login: bool) -> Result<()> {
        if !self.config.definition_path(self.platform).is_file() {
            return Ok(());
        }
        self.require_installed()?;
        match self.platform {
            NativeServicePlatform::Macos => {
                let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
                let loaded =
                    self.run_allow_failure("launchctl", &["print".into(), target.clone().into()])?;
                ensure_launchd_print_result(&loaded)?;
                if loaded.success {
                    self.run_checked("launchctl", &["bootout".into(), target.clone().into()])?;
                    self.wait_unloaded_or_inactive()?;
                }
            }
            NativeServicePlatform::Linux => {
                self.run_checked(
                    "systemctl",
                    &["--user".into(), "stop".into(), SYSTEMD_UNIT.into()],
                )?;
                self.wait_unloaded_or_inactive()?;
            }
        }
        if disable_at_login && self.config.definition_path(self.platform).exists() {
            self.set_enabled(false)?;
        }
        Ok(())
    }

    pub fn restart(&self) -> Result<()> {
        let enabled = self.status()?.enabled;
        self.stop(false)?;
        self.start(enabled)
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.require_installed()?;
        match self.platform {
            NativeServicePlatform::Macos => self.set_macos_enabled(enabled),
            NativeServicePlatform::Linux => {
                let verb = if enabled { "enable" } else { "disable" };
                self.run_checked(
                    "systemctl",
                    &["--user".into(), verb.into(), SYSTEMD_UNIT.into()],
                )
            }
        }
    }

    pub fn status(&self) -> Result<NativeServiceStatus> {
        let installed = self.config.definition_path(self.platform).is_file();
        if !installed {
            return Ok(NativeServiceStatus {
                installed: false,
                running: false,
                job_loaded: false,
                enabled: false,
                detail: None,
            });
        }
        self.require_installed()?;
        match self.platform {
            NativeServicePlatform::Macos => {
                let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
                let print =
                    self.run_allow_failure("launchctl", &["print".into(), target.clone().into()])?;
                ensure_launchd_print_result(&print)?;
                let disabled = self.run_allow_failure(
                    "launchctl",
                    &[
                        "print-disabled".into(),
                        format!("gui/{}", self.current_uid()?).into(),
                    ],
                )?;
                if !disabled.success {
                    bail!(
                        "launchctl could not determine whether Gents is enabled: {}",
                        output_detail(&disabled)
                    );
                }
                let explicitly_disabled = launchd_is_disabled(&disabled.stdout)?;
                Ok(NativeServiceStatus {
                    installed,
                    running: print.success && launchd_is_running(&print.stdout),
                    job_loaded: print.success,
                    enabled: !explicitly_disabled,
                    detail: Some(output_detail(&print)),
                })
            }
            NativeServicePlatform::Linux => {
                let active = self.run_allow_failure(
                    "systemctl",
                    &["--user".into(), "is-active".into(), SYSTEMD_UNIT.into()],
                )?;
                let enabled = self.run_allow_failure(
                    "systemctl",
                    &["--user".into(), "is-enabled".into(), SYSTEMD_UNIT.into()],
                )?;
                if !matches!(
                    active.stdout.as_str(),
                    "active"
                        | "inactive"
                        | "failed"
                        | "unknown"
                        | "activating"
                        | "deactivating"
                        | "reloading"
                ) {
                    bail!(
                        "systemctl could not determine whether Gents is running: {}",
                        output_detail(&active)
                    );
                }
                if !enabled.success
                    && !matches!(
                        enabled.stdout.as_str(),
                        "disabled" | "masked" | "static" | "indirect" | "generated"
                    )
                {
                    bail!(
                        "systemctl could not determine whether Gents is enabled: {}",
                        output_detail(&enabled)
                    );
                }
                Ok(NativeServiceStatus {
                    installed,
                    running: active.success && active.stdout == "active",
                    job_loaded: matches!(
                        active.stdout.as_str(),
                        "active" | "activating" | "deactivating" | "reloading"
                    ),
                    enabled: enabled.success && enabled.stdout == "enabled",
                    detail: Some(output_detail(&active)),
                })
            }
        }
    }

    pub fn uninstall(&self) -> Result<()> {
        // Service definitions are disposable; the configured Gents home and
        // all runtime data deliberately remain untouched.
        let path = self.config.definition_path(self.platform);
        if !path.exists() {
            return Ok(());
        }
        self.stop(true)?;
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        if self.platform == NativeServicePlatform::Linux {
            self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
        }
        Ok(())
    }

    fn require_installed(&self) -> Result<()> {
        let path = self.config.definition_path(self.platform);
        if !path.is_file() {
            bail!(
                "native service is not installed at {}; run `gents service install` first",
                path.display()
            );
        }
        let installed = fs::read(&path)
            .with_context(|| format!("reading native service definition {}", path.display()))?;
        self.ensure_owned_definition(&installed)?;
        Ok(())
    }

    fn ensure_owned_definition(&self, definition: &[u8]) -> Result<()> {
        let installed_home = normalize_absolute(&installed_home(self.platform, definition)?)?;
        // `with_runner` deliberately accepts fully injected configs for tests
        // and embedders, so normalize the caller side at the ownership
        // boundary too. On macOS this notably makes /var and /private/var the
        // same home without weakening the fixed-label cross-home check.
        let requested_home = normalize_absolute(&self.config.home)?;
        if installed_home != requested_home {
            bail!(
                "native service belongs to Gents home {}, not {}; refusing to control it",
                installed_home.display(),
                requested_home.display()
            );
        }
        Ok(())
    }

    fn render_definition(&self) -> Result<String> {
        match self.platform {
            NativeServicePlatform::Macos => render_launchd(&self.config),
            NativeServicePlatform::Linux => render_systemd(&self.config),
        }
    }

    /// Confirm the service-manager transition: launchd no longer has the job
    /// label loaded, or systemd reports the unit inactive. This does not claim
    /// an independent observation of the former child PID.
    fn wait_unloaded_or_inactive(&self) -> Result<()> {
        for _ in 0..20 {
            match self.platform {
                NativeServicePlatform::Macos => {
                    let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
                    let output =
                        self.run_allow_failure("launchctl", &["print".into(), target.into()])?;
                    ensure_launchd_print_result(&output)?;
                    if !output.success {
                        return Ok(());
                    }
                }
                NativeServicePlatform::Linux => {
                    let output = self.run_allow_failure(
                        "systemctl",
                        &["--user".into(), "is-active".into(), SYSTEMD_UNIT.into()],
                    )?;
                    match output.stdout.as_str() {
                        "inactive" | "failed" | "unknown" => return Ok(()),
                        "active" | "activating" | "deactivating" | "reloading" => {}
                        _ => bail!(
                            "systemctl could not confirm that the Gents unit became inactive: {}",
                            output_detail(&output)
                        ),
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        bail!("native supervisor did not confirm that the Gents job unloaded or became inactive")
    }

    fn run_checked(&self, program: &str, args: &[OsString]) -> Result<()> {
        let output = self.runner.run(OsStr::new(program), args)?;
        if !output.success {
            bail!("{program} failed: {}", output_detail(&output));
        }
        Ok(())
    }

    fn current_uid(&self) -> Result<u32> {
        let output = self.runner.run(OsStr::new("/usr/bin/id"), &["-u".into()])?;
        if !output.success {
            bail!(
                "could not resolve current user id: {}",
                output_detail(&output)
            );
        }
        output
            .stdout
            .trim()
            .parse()
            .context("parsing current user id")
    }

    fn set_macos_enabled(&self, enabled: bool) -> Result<()> {
        let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
        let verb = if enabled { "enable" } else { "disable" };
        self.run_checked("launchctl", &[verb.into(), target.into()])
    }

    fn run_allow_failure(&self, program: &str, args: &[OsString]) -> Result<CommandOutput> {
        self.runner.run(OsStr::new(program), args).with_context(|| {
            if self.platform == NativeServicePlatform::Linux {
                "systemd user services are unavailable; a user systemd session is required (Gents does not change linger or root settings)"
            } else {
                "launchd user services are unavailable"
            }
        })
    }
}

fn validate_config(config: &NativeServiceConfig) -> Result<()> {
    if !config.executable.is_absolute() {
        bail!("native service executable must be an absolute path");
    }
    if !config.home.is_absolute() {
        bail!("Gents service home must be an absolute path");
    }
    if !config.home.is_dir() {
        bail!(
            "Gents service home does not exist at {}",
            config.home.display()
        );
    }
    if !config.executable.is_file() {
        bail!(
            "Gents executable does not exist at {}",
            config.executable.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if fs::metadata(&config.executable)?.permissions().mode() & 0o111 == 0 {
            bail!(
                "Gents executable is not executable at {}",
                config.executable.display()
            );
        }
    }
    Ok(())
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("native service paths must be absolute");
    }
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .context("absolute path has no existing ancestor")?;
        missing.push(name.to_owned());
        existing = existing
            .parent()
            .context("absolute path has no existing ancestor")?;
    }
    let mut normalized = fs::canonicalize(existing)
        .with_context(|| format!("canonicalizing path ancestor {}", existing.display()))?;
    for component in missing.into_iter().rev() {
        if component == OsStr::new(".") || component == OsStr::new("..") {
            bail!("native service path contains unresolved traversal");
        }
        normalized.push(component);
    }
    Ok(normalized)
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("service definition has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write as _;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn render_launchd(config: &NativeServiceConfig) -> Result<String> {
    let executable = xml_path(&config.executable, "executable")?;
    let home = xml_path(&config.home, "home")?;
    let search_path = config
        .search_path
        .as_ref()
        .map(|value| -> Result<String> {
            let value = value.to_str().context("PATH is not valid UTF-8")?;
            Ok(format!(
                "<key>PATH</key><string>{}</string>",
                xml_escape(value)?
            ))
        })
        .transpose()?
        .unwrap_or_default();
    let environment = format!("\n  <key>EnvironmentVariables</key>\n  <dict><key>GENTS_SYSTEM_LOG</key><string>1</string>{search_path}</dict>");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{SERVICE_LABEL}</string>
  <key>ProgramArguments</key><array>
    <string>{executable}</string><string>server</string><string>--home</string><string>{home}</string>
  </array>{environment}
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Background</string>
</dict></plist>
"#
    ))
}

fn render_systemd(config: &NativeServiceConfig) -> Result<String> {
    let executable = systemd_arg(
        config
            .executable
            .to_str()
            .context("executable path is not valid UTF-8")?,
    )?;
    let home = systemd_arg(
        config
            .home
            .to_str()
            .context("home path is not valid UTF-8")?,
    )?;
    let search_path = config
        .search_path
        .as_ref()
        .map(|value| systemd_environment("PATH", &value.to_string_lossy()))
        .transpose()?
        .map(|value| format!("Environment={value}\n"))
        .unwrap_or_default();
    Ok(format!("[Unit]\nDescription=Gents agent runtime\n\n[Service]\nType=simple\nExecStart={executable} \"server\" \"--home\" {home}\nEnvironment=\"GENTS_SYSTEM_LOG=1\"\n{search_path}StandardOutput=journal\nStandardError=journal\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n"))
}

fn xml_path(path: &Path, name: &str) -> Result<String> {
    let value = path
        .to_str()
        .with_context(|| format!("{name} path is not valid UTF-8"))?;
    xml_escape(value)
}

fn xml_escape(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|ch| matches!(ch, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}'))
    {
        bail!("value contains a character forbidden by XML 1.0");
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn systemd_arg(value: &str) -> Result<String> {
    systemd_quote(value, true)
}

fn systemd_quote(value: &str, escape_dollar: bool) -> Result<String> {
    if value
        .chars()
        .any(|ch| ch == '\n' || ch == '\r' || ch == '\0')
    {
        bail!("service argument contains an unsupported control character");
    }
    let value = value
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let value = if escape_dollar {
        value.replace('$', "$$")
    } else {
        value
    };
    Ok(format!("\"{value}\""))
}

fn systemd_environment(name: &str, value: &str) -> Result<String> {
    systemd_quote(&format!("{name}={value}"), false)
}

fn installed_home(platform: NativeServicePlatform, definition: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(definition).context("native service definition is not UTF-8")?;
    let home = match platform {
        NativeServicePlatform::Macos => launchd_home(text)?,
        NativeServicePlatform::Linux => systemd_home(text)?,
    };
    Ok(PathBuf::from(home))
}

fn launchd_home(plist: &str) -> Result<String> {
    let value = plist::Value::from_reader_xml(std::io::Cursor::new(plist.as_bytes()))
        .context("parsing launchd property list")?;
    let dictionary = value
        .as_dictionary()
        .context("launchd definition root is not a dictionary")?;
    if dictionary.get("Label").and_then(plist::Value::as_string) != Some(SERVICE_LABEL) {
        bail!("launchd definition does not have the Gents service label");
    }
    let arguments = dictionary
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .context("launchd definition has no ProgramArguments array")?;
    let values = arguments
        .iter()
        .map(|value| {
            value
                .as_string()
                .context("launchd argument is not a string")
        })
        .collect::<Result<Vec<_>>>()?;
    if values.get(1).copied() != Some("server")
        || values.get(2).copied() != Some("--home")
        || values.len() != 4
    {
        bail!("launchd definition is not a Gents foreground server invocation");
    }
    Ok(values[3].to_owned())
}

fn systemd_home(unit: &str) -> Result<String> {
    let exec = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .context("systemd definition has no ExecStart")?;
    let marker = " \"server\" \"--home\" ";
    let (_, home) = exec
        .split_once(marker)
        .context("systemd definition is not a Gents foreground server invocation")?;
    parse_systemd_quoted(home)
}

fn parse_systemd_quoted(value: &str) -> Result<String> {
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .context("systemd home argument is not quoted")?;
    let mut decoded = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next().context("trailing systemd escape")? {
                '\\' => decoded.push('\\'),
                '"' => decoded.push('"'),
                _ => bail!("unsupported escape in systemd home argument"),
            },
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                decoded.push('%');
            }
            '$' if chars.peek() == Some(&'$') => {
                chars.next();
                decoded.push('$');
            }
            '%' | '$' | '"' | '\n' | '\r' => {
                bail!("unescaped character in systemd home argument")
            }
            _ => decoded.push(ch),
        }
    }
    Ok(decoded)
}

fn output_detail(output: &CommandOutput) -> String {
    if !output.stderr.is_empty() {
        output.stderr.clone()
    } else if !output.stdout.is_empty() {
        output.stdout.clone()
    } else if output.success {
        "ok".into()
    } else {
        "command exited unsuccessfully".into()
    }
}

fn launchd_is_running(output: &str) -> bool {
    let state_running = output
        .lines()
        .map(str::trim)
        .any(|line| line == "state = running");
    let has_pid = output.lines().map(str::trim).any(|line| {
        line.strip_prefix("pid = ")
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| pid > 0)
    });
    state_running && has_pid
}

fn launchd_is_disabled(output: &str) -> Result<bool> {
    for line in output.lines() {
        let Some((label, value)) = line.split_once("=>") else {
            continue;
        };
        if label.trim().trim_matches('"') != SERVICE_LABEL {
            continue;
        }
        // launchctl uses enabled/disabled on current macOS; older versions
        // render the disabled override as a boolean. Absence means no override.
        return match value.trim() {
            "true" | "disabled" => Ok(true),
            "false" | "enabled" => Ok(false),
            value => bail!("launchctl returned an unknown Gents disabled override: {value}"),
        };
    }
    Ok(false)
}

fn ensure_launchd_print_result(output: &CommandOutput) -> Result<()> {
    if output.success
        || output.stderr.contains("Could not find service")
        || output.stdout.contains("Could not find service")
    {
        Ok(())
    } else {
        bail!(
            "launchctl could not inspect Gents: {}",
            output_detail(output)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn launchd_disabled_override_accepts_native_and_boolean_formats() {
        for value in ["disabled", "true"] {
            assert!(launchd_is_disabled(&format!(
                "disabled services = {{\n  \"{SERVICE_LABEL}\" => {value}\n}}"
            ))
            .unwrap());
        }
        for value in ["enabled", "false"] {
            assert!(!launchd_is_disabled(&format!("\"{SERVICE_LABEL}\" => {value}")).unwrap());
        }
        assert!(!launchd_is_disabled(
            "disabled services = {\n  \"another.service\" => disabled\n}"
        )
        .unwrap());
        assert!(launchd_is_disabled(&format!("\"{SERVICE_LABEL}\" => unknown")).is_err());
    }

    struct OrderedRunner(Mutex<Vec<CommandOutput>>);

    impl CommandRunner for OrderedRunner {
        fn run(&self, _: &OsStr, _: &[OsString]) -> Result<CommandOutput> {
            Ok(self.0.lock().unwrap().remove(0))
        }
    }

    fn command_output(success: bool, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn native_command_timeout_is_long_only_for_stop_operations() {
        assert_eq!(
            command_timeout(
                OsStr::new("systemctl"),
                &["--user".into(), "stop".into(), SYSTEMD_UNIT.into()]
            ),
            Duration::from_secs(120)
        );
        assert_eq!(
            command_timeout(
                OsStr::new("launchctl"),
                &["bootout".into(), format!("gui/501/{SERVICE_LABEL}").into()]
            ),
            Duration::from_secs(120)
        );
        for (program, args) in [
            (
                "systemctl",
                vec!["--user".into(), "start".into(), SYSTEMD_UNIT.into()],
            ),
            (
                "systemctl",
                vec!["--user".into(), "is-active".into(), SYSTEMD_UNIT.into()],
            ),
            (
                "launchctl",
                vec![
                    "kickstart".into(),
                    format!("gui/501/{SERVICE_LABEL}").into(),
                ],
            ),
        ] {
            assert_eq!(
                command_timeout(OsStr::new(program), &args),
                Duration::from_secs(10)
            );
        }
    }

    fn config(root: &Path) -> NativeServiceConfig {
        let executable = root.join("gents & runtime");
        fs::write(&executable, b"test").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        NativeServiceConfig {
            home: root.join("agent home"),
            executable,
            user_home: root.to_owned(),
            service_config_dir: root.join("service-config"),
            search_path: Some("/a path/bin:/usr/bin".into()),
        }
    }

    #[test]
    fn launchd_definition_uses_arguments_and_escapes_xml() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        let plist = render_launchd(&config).unwrap();
        assert!(plist.contains("gents &amp; runtime"));
        assert!(plist.contains("<string>--home</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
        assert!(!plist.contains("sh -c"));
        assert_eq!(launchd_home(&plist).unwrap(), config.home.to_string_lossy());
    }

    #[test]
    fn systemd_definition_escapes_specifiers_and_quotes() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = config(temp.path());
        config.home = temp.path().join("agent % $ home\" \\ path");
        let unit = render_systemd(&config).unwrap();
        assert!(unit.contains("agent %% $$ home\\\" \\\\ path"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(!unit.contains("sh -c"));
        assert_eq!(systemd_home(&unit).unwrap(), config.home.to_string_lossy());
    }

    #[test]
    fn systemd_home_parser_rejects_injected_or_malformed_arguments() {
        for unit in [
            "ExecStart=\"/bin/gents\" \"server\" \"--home\" \"/safe\" --extra",
            "ExecStart=\"/bin/gents\" \"server\" \"--home\" \"/bad%path\"",
            "ExecStart=\"/bin/gents\" \"server\" \"--home\" \"/bad\\npath\"",
            "ExecStart=\"/bin/gents\" \"chat\" \"--home\" \"/safe\"",
        ] {
            assert!(systemd_home(unit).is_err(), "accepted {unit:?}");
        }
    }

    #[test]
    fn install_writes_disabled_definition_and_preserves_data() {
        struct SuccessRunner;
        impl CommandRunner for SuccessRunner {
            fn run(&self, program: &OsStr, _: &[OsString]) -> Result<CommandOutput> {
                Ok(CommandOutput {
                    success: true,
                    stdout: if program == OsStr::new("/usr/bin/id") {
                        "501".into()
                    } else {
                        String::new()
                    },
                    stderr: String::new(),
                })
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        fs::create_dir_all(&config.home).unwrap();
        fs::write(config.home.join("keep"), b"data").unwrap();
        let manager = NativeServiceManager::with_runner(
            config.clone(),
            NativeServicePlatform::Macos,
            SuccessRunner,
        );
        manager.install().unwrap();
        assert!(config
            .definition_path(NativeServicePlatform::Macos)
            .is_file());
        assert_eq!(fs::read(config.home.join("keep")).unwrap(), b"data");
    }

    #[test]
    fn linux_lifecycle_uses_supervisor_state_and_preserves_home_data() {
        struct QueueRunner {
            outputs: Mutex<Vec<CommandOutput>>,
            calls: Mutex<Vec<Vec<String>>>,
        }
        impl CommandRunner for QueueRunner {
            fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
                let mut call = vec![program.to_string_lossy().into_owned()];
                call.extend(args.iter().map(|arg| arg.to_string_lossy().into_owned()));
                self.calls.lock().unwrap().push(call);
                Ok(self.outputs.lock().unwrap().remove(0))
            }
        }
        fn output(success: bool, stdout: &str) -> CommandOutput {
            CommandOutput {
                success,
                stdout: stdout.into(),
                stderr: String::new(),
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        fs::create_dir_all(&config.home).unwrap();
        fs::write(config.home.join("keep"), b"durable").unwrap();
        let runner = QueueRunner {
            // install reload+disable, status active+enabled, stop+inactive,
            // uninstall stop+inactive+disable+reload
            outputs: Mutex::new(vec![
                output(true, ""),
                output(true, ""),
                output(true, "active"),
                output(true, "enabled"),
                output(true, ""),
                output(false, "inactive"),
                output(true, ""),
                output(false, "inactive"),
                output(true, ""),
                output(true, ""),
            ]),
            calls: Mutex::new(Vec::new()),
        };
        let manager =
            NativeServiceManager::with_runner(config.clone(), NativeServicePlatform::Linux, runner);
        manager.install().unwrap();
        let status = manager.status().unwrap();
        assert!(status.running && status.enabled);
        manager.stop(false).unwrap();
        manager.uninstall().unwrap();
        assert!(!config
            .definition_path(NativeServicePlatform::Linux)
            .exists());
        assert_eq!(fs::read(config.home.join("keep")).unwrap(), b"durable");
    }

    #[test]
    fn fixed_label_refuses_to_control_a_different_home() {
        struct SuccessRunner;
        impl CommandRunner for SuccessRunner {
            fn run(&self, _: &OsStr, _: &[OsString]) -> Result<CommandOutput> {
                Ok(CommandOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let owner = config(temp.path());
        fs::create_dir_all(&owner.home).unwrap();
        let owner_manager = NativeServiceManager::with_runner(
            owner.clone(),
            NativeServicePlatform::Linux,
            SuccessRunner,
        );
        owner_manager.install().unwrap();

        let mut foreign = owner.clone();
        foreign.home = temp.path().join("other-home");
        fs::create_dir_all(&foreign.home).unwrap();
        let foreign_manager =
            NativeServiceManager::with_runner(foreign, NativeServicePlatform::Linux, SuccessRunner);
        assert!(foreign_manager
            .status()
            .unwrap_err()
            .to_string()
            .contains("belongs to Gents home"));
    }

    #[test]
    fn supervisor_inspection_failure_after_stop_is_not_treated_as_stopped() {
        struct QueueRunner(Mutex<Vec<CommandOutput>>);
        impl CommandRunner for QueueRunner {
            fn run(&self, _: &OsStr, _: &[OsString]) -> Result<CommandOutput> {
                Ok(self.0.lock().unwrap().remove(0))
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        fs::create_dir_all(&config.home).unwrap();
        fs::create_dir_all(&config.service_config_dir).unwrap();
        fs::write(
            config.definition_path(NativeServicePlatform::Linux),
            render_systemd(&config).unwrap(),
        )
        .unwrap();
        let runner = QueueRunner(Mutex::new(vec![
            CommandOutput {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            },
            CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "Failed to connect to bus".into(),
            },
        ]));
        let manager =
            NativeServiceManager::with_runner(config, NativeServicePlatform::Linux, runner);
        assert!(manager
            .stop(false)
            .unwrap_err()
            .to_string()
            .contains("could not confirm"));
    }

    #[test]
    fn linux_status_reports_transitions_without_treating_them_as_running() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        fs::create_dir_all(&config.home).unwrap();
        fs::create_dir_all(&config.service_config_dir).unwrap();
        fs::write(
            config.definition_path(NativeServicePlatform::Linux),
            render_systemd(&config).unwrap(),
        )
        .unwrap();
        let manager = NativeServiceManager::with_runner(
            config,
            NativeServicePlatform::Linux,
            OrderedRunner(Mutex::new(vec![
                command_output(false, "activating", ""),
                command_output(true, "enabled", ""),
            ])),
        );
        let status = manager.status().unwrap();
        assert!(!status.running);
        assert_eq!(status.detail.as_deref(), Some("activating"));
    }

    #[test]
    fn install_does_not_replace_definition_while_linux_is_transitioning() {
        let temp = tempfile::tempdir().unwrap();
        let original = config(temp.path());
        fs::create_dir_all(&original.home).unwrap();
        fs::create_dir_all(&original.service_config_dir).unwrap();
        let original_definition = render_systemd(&original).unwrap();
        let definition_path = original.definition_path(NativeServicePlatform::Linux);
        fs::write(&definition_path, &original_definition).unwrap();

        let mut replacement = original.clone();
        replacement.search_path = Some("/different/bin".into());
        let manager = NativeServiceManager::with_runner(
            replacement,
            NativeServicePlatform::Linux,
            OrderedRunner(Mutex::new(vec![
                command_output(false, "deactivating", ""),
                command_output(true, "enabled", ""),
            ])),
        );
        assert!(manager
            .install()
            .unwrap_err()
            .to_string()
            .contains("stop it"));
        assert_eq!(
            fs::read_to_string(definition_path).unwrap(),
            original_definition
        );
    }

    #[test]
    fn fresh_macos_install_disables_before_writing_plist() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        fs::create_dir_all(&config.home).unwrap();
        let definition = config.definition_path(NativeServicePlatform::Macos);
        let manager = NativeServiceManager::with_runner(
            config,
            NativeServicePlatform::Macos,
            OrderedRunner(Mutex::new(vec![
                command_output(true, "501", ""),
                command_output(false, "", "disable denied"),
            ])),
        );
        assert!(manager.install().is_err());
        assert!(!definition.exists());
    }

    #[test]
    fn macos_start_preserves_start_and_restore_failures() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path());
        fs::create_dir_all(&config.home).unwrap();
        fs::create_dir_all(&config.service_config_dir).unwrap();
        fs::write(
            config.definition_path(NativeServicePlatform::Macos),
            render_launchd(&config).unwrap(),
        )
        .unwrap();
        let manager = NativeServiceManager::with_runner(
            config,
            NativeServicePlatform::Macos,
            OrderedRunner(Mutex::new(vec![
                command_output(true, "501", ""),
                command_output(false, "", "Could not find service"),
                command_output(true, "501", ""),
                command_output(true, "\"ai.gents.runtime\" => disabled", ""),
                command_output(true, "501", ""),
                command_output(true, "", ""),
                command_output(true, "501", ""),
                command_output(true, "501", ""),
                command_output(true, "", ""),
                command_output(false, "", "start boom"),
                command_output(true, "501", ""),
                command_output(false, "", "disable boom"),
            ])),
        );
        let error = manager.start(false).unwrap_err().to_string();
        assert!(error.contains("start boom"));
        assert!(error.contains("disable boom"));
        assert!(error.contains("enablement is uncertain"));
    }
}
