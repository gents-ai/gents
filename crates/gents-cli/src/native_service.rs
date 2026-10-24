use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use std::io;

use gents_protocol::error_chain::ErrorChain;
#[cfg(not(all(feature = "native-e2e", debug_assertions)))]
pub const SERVICE_LABEL: &str = "ai.gents.runtime";
#[cfg(all(feature = "native-e2e", debug_assertions))]
pub const SERVICE_LABEL: &str = "ai.gents.runtime.native-e2e";
#[cfg(not(all(feature = "native-e2e", debug_assertions)))]
pub const SYSTEMD_UNIT: &str = "gents-runtime.service";
#[cfg(all(feature = "native-e2e", debug_assertions))]
pub const SYSTEMD_UNIT: &str = "gents-runtime-native-e2e.service";
/// Exit status of `gents` when it refuses to open a store this build cannot
/// read (`gents::storage_backend::IncompatibleStore`). Supervisors and the
/// desktop read it from the exit record to tell that refusal apart from a
/// crash; systemd does not restart it. It is sysexits `EX_DATAERR`.
pub const INCOMPATIBLE_STORE_EXIT_CODE: i32 = 65;
/// Like [`INCOMPATIBLE_STORE_EXIT_CODE`], for a store another (possibly
/// newer) build extended rather than an older one (sysexits `EX_NOUSER`, an
/// otherwise unused status here).
pub const FOREIGN_STORE_EXIT_CODE: i32 = 67;
/// Like [`INCOMPATIBLE_STORE_EXIT_CODE`], for an identity key an older build
/// wrote with unsafe permissions (sysexits `EX_NOINPUT`).
pub const INSECURE_KEY_EXIT_CODE: i32 = 66;

pub fn incompatible_store_exit_code(kind: gents::storage_backend::IncompatibleStoreKind) -> i32 {
    use gents::storage_backend::IncompatibleStoreKind;
    match kind {
        IncompatibleStoreKind::InsecureKey => INSECURE_KEY_EXIT_CODE,
        IncompatibleStoreKind::ForeignVersion => FOREIGN_STORE_EXIT_CODE,
        _ => INCOMPATIBLE_STORE_EXIT_CODE,
    }
}
pub const BACKGROUND_APPROVAL_REQUIRED: &str = "macOS has not allowed Gents to run in the background. Approve Gents in System Settings > General > Login Items & Extensions, then start it again.";

/// A start that macOS refused because the background item is not approved.
/// Callers wait for approval and start again instead of failing. `detail` is
/// launchd's refusal, when there was one.
#[derive(Debug)]
pub struct BackgroundApprovalPending {
    pub detail: String,
}

impl std::fmt::Display for BackgroundApprovalPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(BACKGROUND_APPROVAL_REQUIRED)
    }
}

impl std::error::Error for BackgroundApprovalPending {}

#[derive(Debug, thiserror::Error)]
pub enum NativeServiceError {
    #[error(transparent)]
    ApprovalPending(#[from] BackgroundApprovalPending),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("native Gents services are supported only on macOS and Linux")]
    UnsupportedPlatform,
    #[error("HOME is required to manage a per-user native service")]
    HomeRequired,
    #[error("native service executable must be an absolute path")]
    ExecutableNotAbsolute,
    #[error("resolving Gents executable {}", path.display())]
    ResolveExecutable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the Gents executable at {} is inside a temporary AppImage mount and cannot be recorded as a service executable. Open the desktop app so it can install a durable runtime, or pass --executable pointing at a .deb or extracted install.", path.display())]
    ExecutableInTemporaryPackage { path: PathBuf },
    #[error("creating native command stdout capture")]
    CaptureStdout(#[source] io::Error),
    #[error("creating native command stderr capture")]
    CaptureStderr(#[source] io::Error),
    #[error("executing {}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("waiting for native service command")]
    Wait(#[source] io::Error),
    #[error("native service command timed out after {seconds} seconds{cleanup}")]
    CommandTimedOut { seconds: u64, cleanup: String },
    #[error("this version of macOS cannot open Login Items settings directly")]
    LoginItemsUnavailable,
    #[error("background item approval exists only on macOS")]
    BackgroundApprovalUnsupported,
    #[error("native service definition has no parent")]
    DefinitionHasNoParent,
    #[error("creating service directory {}", path.display())]
    CreateServiceDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reading installed native service {}", path.display())]
    ReadInstalledDefinition {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reading native service definition {}", path.display())]
    ReadDefinition {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("native service definition is not UTF-8")]
    DefinitionNotUtf8(#[source] std::str::Utf8Error),
    #[error("native service is running or transitioning; stop it before updating its executable or environment")]
    ServiceActiveDuringUpdate,
    #[error("updating native service definition {}", path.display())]
    UpdateDefinition {
        path: PathBuf,
        #[source]
        source: AtomicWriteError,
    },
    #[error("writing native service definition {}", path.display())]
    WriteDefinition {
        path: PathBuf,
        #[source]
        source: AtomicWriteError,
    },
    #[error(
        "Gents started, but restoring disabled-at-login state failed; enablement is uncertain"
    )]
    StartedButEnablementUncertain {
        #[source]
        source: Box<NativeServiceError>,
    },
    #[error("launchctl could not start Gents: {kick} (bootstrap: {bootstrap})")]
    LaunchctlStart { kick: String, bootstrap: String },
    #[error("Gents failed to start: {start}; restoring disabled-at-login state also failed, so enablement is uncertain: {restore}")]
    StartAndRestoreFailed { start: String, restore: String },
    #[error("launchctl could not determine whether Gents is enabled: {detail}")]
    LaunchctlEnabledUnknown { detail: String },
    #[error("launchctl returned an unknown Gents disabled override: {value}")]
    LaunchctlUnknownOverride { value: String },
    #[error("systemctl could not determine whether Gents is running: {detail}")]
    SystemctlRunningUnknown { detail: String },
    #[error("systemctl could not determine whether Gents is enabled: {detail}")]
    SystemctlEnabledUnknown { detail: String },
    #[error("systemctl could not report why Gents stopped: {detail}")]
    SystemctlExitUnknown { detail: String },
    #[error("removing {}", path.display())]
    RemoveDefinition {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("service log path has no parent")]
    LogHasNoParent,
    #[error("creating service log directory {}", path.display())]
    CreateLogDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("creating service log {}", path.display())]
    CreateLog {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("restricting service log directory {}", path.display())]
    RestrictLogDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("restricting service log {}", path.display())]
    RestrictLog {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("native service is not installed at {}; run `gents service install` first", path.display())]
    NotInstalled { path: PathBuf },
    #[error("native service belongs to Gents home {}, not {}; refusing to control it", installed.display(), requested.display())]
    HomeMismatch {
        installed: PathBuf,
        requested: PathBuf,
    },
    #[error("systemctl could not confirm that the Gents unit became inactive: {detail}")]
    SystemctlNotInactive { detail: String },
    #[error("native supervisor did not confirm that the Gents job unloaded or became inactive")]
    UnloadNotConfirmed,
    #[error("{program} failed: {detail}")]
    CommandFailed { program: String, detail: String },
    #[error("could not resolve current user id: {detail}")]
    CurrentUidUnavailable { detail: String },
    #[error("parsing current user id")]
    ParseUid(#[source] std::num::ParseIntError),
    #[error("{message}")]
    ServiceManagerUnavailable {
        message: &'static str,
        #[source]
        source: Box<NativeServiceError>,
    },
    #[error("native service paths must be absolute")]
    PathNotAbsolute,
    #[error("Gents service home must be an absolute path")]
    HomeNotAbsolute,
    #[error("Gents service home does not exist at {}", path.display())]
    HomeMissing { path: PathBuf },
    #[error("Gents executable does not exist at {}", path.display())]
    ExecutableMissing { path: PathBuf },
    #[error("Gents executable is not executable at {}", path.display())]
    ExecutableNotExecutable { path: PathBuf },
    #[error("absolute path has no existing ancestor")]
    NoExistingAncestor,
    #[error("canonicalizing path ancestor {}", path.display())]
    CanonicalizeAncestor {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("native service path contains unresolved traversal")]
    UnresolvedTraversal,
    #[error("PATH is not valid UTF-8")]
    SearchPathNotUtf8,
    #[error("{name} path is not valid UTF-8")]
    PathNotUtf8 { name: &'static str },
    #[error("value contains a character forbidden by XML 1.0")]
    XmlForbiddenCharacter,
    #[error("executable path is not valid UTF-8")]
    ExecutablePathNotUtf8,
    #[error("home path is not valid UTF-8")]
    HomePathNotUtf8,
    #[error("stderr log path is not valid UTF-8")]
    StderrLogPathNotUtf8,
    #[error("stderr log path contains an unsupported control character")]
    StderrLogControlCharacter,
    #[error("service argument contains an unsupported control character")]
    ServiceArgumentControlCharacter,
    #[error("parsing launchd property list")]
    ParseLaunchdPlist(#[source] plist::Error),
    #[error("launchd definition root is not a dictionary")]
    LaunchdRootNotDictionary,
    #[error("launchd definition does not have the Gents service label")]
    LaunchdLabelMismatch,
    #[error("launchd definition has no ProgramArguments array")]
    LaunchdNoProgramArguments,
    #[error("launchd argument is not a string")]
    LaunchdArgumentNotString,
    #[error("launchd definition is not a Gents foreground server invocation")]
    LaunchdNotForegroundServer,
    #[error("systemd definition has no ExecStart")]
    SystemdNoExecStart,
    #[error("systemd definition is not a Gents foreground server invocation")]
    SystemdNotForegroundServer,
    #[error("systemd home argument is not quoted")]
    SystemdArgumentNotQuoted,
    #[error("trailing systemd escape")]
    SystemdTrailingEscape,
    #[error("unsupported escape in systemd home argument")]
    SystemdUnsupportedEscape,
    #[error("unescaped character in systemd home argument")]
    SystemdUnescapedCharacter,
    #[error("launchctl could not inspect Gents: {detail}")]
    LaunchctlInspectFailed { detail: String },
}

#[derive(Debug, thiserror::Error)]
pub enum AtomicWriteError {
    #[error("path has no parent")]
    NoParent,
    #[error(transparent)]
    Io(#[from] io::Error),
}

type Result<T, E = NativeServiceError> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BackgroundItemStatus {
    #[default]
    Unavailable,
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}

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
            Err(NativeServiceError::UnsupportedPlatform)
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeServiceConfig {
    pub home: PathBuf,
    pub executable: PathBuf,
    pub user_home: PathBuf,
    pub service_config_dir: PathBuf,
    pub search_path: Option<OsString>,
    pub associated_bundle_id: Option<String>,
    /// Where the service manager appends the runtime's stderr. With the
    /// native log sink enabled that holds only what precedes or escapes
    /// tracing: argument errors, the exit error and panics.
    pub stderr_path: Option<PathBuf>,
}

impl NativeServiceConfig {
    pub fn new(home: PathBuf, executable: PathBuf) -> Result<Self> {
        let user_home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(NativeServiceError::HomeRequired)?;
        let home = normalize_absolute(&home)?;
        if !executable.is_absolute() {
            return Err(NativeServiceError::ExecutableNotAbsolute);
        }
        let executable = if executable.exists() {
            fs::canonicalize(&executable).map_err(|source| {
                NativeServiceError::ResolveExecutable {
                    path: executable.clone(),
                    source,
                }
            })?
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
            search_path: forwarded_search_path(
                std::env::var_os("PATH").as_deref(),
                std::env::var_os("APPDIR").as_deref().map(Path::new),
                std::env::var_os("APPIMAGE").as_deref().map(Path::new),
            ),
            associated_bundle_id: None,
            stderr_path: None,
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

/// A service definition must never record a path that lives inside a package
/// mount that exists only while the application that launched it runs, such
/// as an AppImage's mount.
pub fn inside_temporary_package(
    path: &Path,
    app_dir: Option<&Path>,
    app_image: Option<&Path>,
) -> bool {
    // An extracted AppRun also exports APPDIR. Only an active AppImage
    // launcher (APPIMAGE present) makes that directory temporary.
    if app_image.is_none() {
        return false;
    }
    app_dir.is_some_and(|root| path.starts_with(root))
        || path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .starts_with(".mount_")
        })
}

/// The search path a service definition records. Entries inside a temporary
/// package mount are dropped: leaving one in a persistent definition also
/// lets a later process occupy the vacated name in a shared temporary
/// directory and be found ahead of the real tool.
fn forwarded_search_path(
    search_path: Option<&OsStr>,
    app_dir: Option<&Path>,
    app_image: Option<&Path>,
) -> Option<OsString> {
    let durable: Vec<PathBuf> = std::env::split_paths(search_path?)
        .filter(|entry| !inside_temporary_package(entry, app_dir, app_image))
        .collect();
    std::env::join_paths(durable)
        .ok()
        .filter(|value| !value.is_empty())
}

pub fn ensure_durable_service_executable(
    executable: &Path,
    app_dir: Option<&Path>,
    app_image: Option<&Path>,
) -> Result<()> {
    if inside_temporary_package(executable, app_dir, app_image) {
        return Err(NativeServiceError::ExecutableInTemporaryPackage {
            path: executable.to_path_buf(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeServiceStatus {
    pub installed: bool,
    pub running: bool,
    /// The native supervisor still owns an active or transitional job. On
    /// macOS this includes loaded jobs that exited and are awaiting respawn.
    pub job_loaded: bool,
    pub enabled: bool,
    /// macOS Background Task Management blocks the job until the user allows
    /// it under Login Items.
    pub requires_approval: bool,
    pub background_item: BackgroundItemStatus,
    /// The supervisor gave up on the job: a systemd unit in its terminal
    /// `failed` state. It is not loaded, and `last_exit` names the cause.
    pub failed: bool,
    pub detail: Option<String>,
}

impl NativeServiceStatus {
    pub fn is_active_or_transitioning(&self) -> bool {
        self.job_loaded
    }

    /// Whether macOS still withholds approval. After launchd refused a start
    /// with [`BackgroundApprovalPending`], a fresh item that Background Task
    /// Management has not registered (or cannot find) is pending too: it only
    /// reports `RequiresApproval` or `Enabled` once the user acts on it.
    pub fn approval_pending(&self, start_refused: bool) -> bool {
        self.requires_approval
            || (start_refused
                && !self.job_loaded
                && matches!(
                    self.background_item,
                    BackgroundItemStatus::NotRegistered | BackgroundItemStatus::NotFound
                ))
    }

    pub fn summary(&self) -> String {
        format!(
            "installed={} running={} job_loaded={} enabled={} requires_approval={} failed={} (native service state only; runtime health is not checked)",
            self.installed,
            self.running,
            self.job_loaded,
            self.enabled,
            self.requires_approval,
            self.failed
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

    fn background_item_status(&self, _definition: &Path) -> BackgroundItemStatus {
        BackgroundItemStatus::Unavailable
    }
}

pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        use wait_timeout::ChildExt as _;

        let mut stdout = tempfile::tempfile().map_err(NativeServiceError::CaptureStdout)?;
        let mut stderr = tempfile::tempfile().map_err(NativeServiceError::CaptureStderr)?;
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
            .map_err(|source| NativeServiceError::Spawn {
                program: PathBuf::from(program),
                source,
            })?;
        let timeout = command_timeout(program, args);
        let waited = match child.wait_timeout(timeout) {
            Ok(waited) => waited,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(NativeServiceError::Wait(error));
            }
        };
        let status = match waited {
            Some(status) => status,
            None => {
                let kill_error = child.kill().err();
                let wait_error = child.wait().err();
                return Err(NativeServiceError::CommandTimedOut {
                    seconds: timeout.as_secs(),
                    cleanup: [
                        kill_error.map(|error| format!("; kill failed: {error}")),
                        wait_error.map(|error| format!("; reap failed: {error}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                });
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

    #[cfg(target_os = "macos")]
    fn background_item_status(&self, definition: &Path) -> BackgroundItemStatus {
        macos_background::item_status(definition)
    }
}

pub fn open_background_approval_settings() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if macos_background::open_login_items() {
            return Ok(());
        }
        Err(NativeServiceError::LoginItemsUnavailable)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(NativeServiceError::BackgroundApprovalUnsupported)
    }
}

#[cfg(target_os = "macos")]
mod macos_background {
    use std::path::Path;

    use super::BackgroundItemStatus;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, Bool};
    use objc2::{msg_send, sel};
    use objc2_foundation::{NSString, NSURL};

    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {}

    fn service_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"SMAppService")
    }

    fn responds(class: &AnyClass, selector: objc2::runtime::Sel) -> bool {
        let responds: Bool = unsafe { msg_send![class, respondsToSelector: selector] };
        responds.as_bool()
    }

    pub(super) fn item_status(definition: &Path) -> BackgroundItemStatus {
        let Some(class) = service_class() else {
            return BackgroundItemStatus::Unavailable;
        };
        if !responds(class, sel!(statusForLegacyURL:)) {
            return BackgroundItemStatus::Unavailable;
        }
        autoreleasepool(|_| {
            let path = NSString::from_str(&definition.to_string_lossy());
            let url = NSURL::fileURLWithPath(&path);
            let status: isize = unsafe { msg_send![class, statusForLegacyURL: &*url] };
            match status {
                0 => BackgroundItemStatus::NotRegistered,
                1 => BackgroundItemStatus::Enabled,
                2 => BackgroundItemStatus::RequiresApproval,
                3 => BackgroundItemStatus::NotFound,
                _ => BackgroundItemStatus::Unavailable,
            }
        })
    }

    pub(super) fn open_login_items() -> bool {
        let Some(class) = service_class() else {
            return false;
        };
        if !responds(class, sel!(openSystemSettingsLoginItems)) {
            return false;
        }
        autoreleasepool(|_| {
            let _: () = unsafe { msg_send![class, openSystemSettingsLoginItems] };
        });
        true
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

impl<R> NativeServiceManager<R>
where
    R: CommandRunner,
{
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
        ensure_durable_service_executable(
            &self.config.executable,
            std::env::var_os("APPDIR").as_deref().map(Path::new),
            std::env::var_os("APPIMAGE").as_deref().map(Path::new),
        )?;
        let path = self.config.definition_path(self.platform);
        let parent = path
            .parent()
            .ok_or(NativeServiceError::DefinitionHasNoParent)?;
        fs::create_dir_all(parent).map_err(|source| {
            NativeServiceError::CreateServiceDirectory {
                path: parent.to_path_buf(),
                source,
            }
        })?;
        self.prepare_stderr_log()?;
        let contents = self.render_definition()?;
        if path.exists() {
            let installed =
                fs::read(&path).map_err(|source| NativeServiceError::ReadInstalledDefinition {
                    path: path.clone(),
                    source,
                })?;
            self.ensure_owned_definition(&installed)?;
            if installed != contents.as_bytes() {
                let status = self.status()?;
                if status.running
                    || (self.platform == NativeServicePlatform::Linux
                        && status.is_active_or_transitioning())
                {
                    return Err(NativeServiceError::ServiceActiveDuringUpdate);
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
                atomic_write(&path, contents.as_bytes()).map_err(|source| {
                    NativeServiceError::UpdateDefinition {
                        path: path.clone(),
                        source,
                    }
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
        atomic_write(&path, contents.as_bytes()).map_err(|source| {
            NativeServiceError::WriteDefinition {
                path: path.clone(),
                source,
            }
        })?;
        // Enablement remains OS-native state rather than a second Gents
        // preference file.
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
        self.prepare_stderr_log()?;
        if self.platform == NativeServicePlatform::Macos
            && self.background_item() == BackgroundItemStatus::RequiresApproval
        {
            return Err(NativeServiceError::ApprovalPending(
                BackgroundApprovalPending {
                    detail: "Background Task Management requires approval".to_string(),
                },
            ));
        }
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
                        if launchd_refused_bootstrap(&bootstrap)
                            && matches!(
                                self.background_item(),
                                BackgroundItemStatus::NotRegistered
                                    | BackgroundItemStatus::NotFound
                            )
                        {
                            return Err(NativeServiceError::ApprovalPending(
                                BackgroundApprovalPending {
                                    detail: output_detail(&bootstrap),
                                },
                            ));
                        }
                        return Err(NativeServiceError::LaunchctlStart {
                            kick: output_detail(&kick),
                            bootstrap: output_detail(&bootstrap),
                        });
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
                    (Ok(()), Err(restore)) => {
                        Err(NativeServiceError::StartedButEnablementUncertain {
                            source: Box::new(restore),
                        })
                    }
                    (Err(start), Err(restore)) => Err(NativeServiceError::StartAndRestoreFailed {
                        start: ErrorChain(&start).to_string(),
                        restore: ErrorChain(&restore).to_string(),
                    }),
                }
            }
            NativeServicePlatform::Linux => {
                self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
                // A unit that hit its start limit refuses `start` until its
                // failed state is cleared. A unit that is not failed ignores this.
                self.run_allow_failure(
                    "systemctl",
                    &["--user".into(), "reset-failed".into(), SYSTEMD_UNIT.into()],
                )?;
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
                // A unit that ends `failed` because of this stop was stopped
                // on purpose; clear it so status does not report a crash.
                self.run_allow_failure(
                    "systemctl",
                    &["--user".into(), "reset-failed".into(), SYSTEMD_UNIT.into()],
                )?;
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
                requires_approval: false,
                background_item: BackgroundItemStatus::Unavailable,
                failed: false,
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
                    return Err(NativeServiceError::LaunchctlEnabledUnknown {
                        detail: output_detail(&disabled),
                    });
                }
                let explicitly_disabled = launchd_is_disabled(&disabled.stdout)?;
                let background_item = self.background_item();
                Ok(NativeServiceStatus {
                    installed,
                    running: print.success && launchd_is_running(&print.stdout),
                    job_loaded: print.success,
                    enabled: !explicitly_disabled,
                    requires_approval: background_item == BackgroundItemStatus::RequiresApproval,
                    background_item,
                    failed: false,
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
                    return Err(NativeServiceError::SystemctlRunningUnknown {
                        detail: output_detail(&active),
                    });
                }
                if !enabled.success
                    && !matches!(
                        enabled.stdout.as_str(),
                        "disabled" | "masked" | "static" | "indirect" | "generated"
                    )
                {
                    return Err(NativeServiceError::SystemctlEnabledUnknown {
                        detail: output_detail(&enabled),
                    });
                }
                Ok(NativeServiceStatus {
                    installed,
                    running: active.success && active.stdout == "active",
                    job_loaded: matches!(
                        active.stdout.as_str(),
                        "active" | "activating" | "deactivating" | "reloading"
                    ),
                    enabled: enabled.success && enabled.stdout == "enabled",
                    requires_approval: false,
                    background_item: BackgroundItemStatus::Unavailable,
                    failed: active.stdout == "failed",
                    detail: Some(output_detail(&active)),
                })
            }
        }
    }

    pub fn last_exit(&self) -> Result<Option<ServiceExit>> {
        if !self.config.definition_path(self.platform).is_file() {
            return Ok(None);
        }
        self.require_installed()?;
        match self.platform {
            NativeServicePlatform::Macos => {
                let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
                let print =
                    self.run_allow_failure("launchctl", &["print".into(), target.into()])?;
                ensure_launchd_print_result(&print)?;
                Ok(print
                    .success
                    .then(|| launchd_exit_failure(&print.stdout))
                    .flatten())
            }
            NativeServicePlatform::Linux => {
                let show = self.run_allow_failure(
                    "systemctl",
                    &[
                        "--user".into(),
                        "show".into(),
                        "--property=ActiveState,SubState,Result,ExecMainStatus,NRestarts".into(),
                        SYSTEMD_UNIT.into(),
                    ],
                )?;
                if !show.success {
                    return Err(NativeServiceError::SystemctlExitUnknown {
                        detail: output_detail(&show),
                    });
                }
                Ok(systemd_exit_failure(&show.stdout))
            }
        }
    }

    pub fn installed_executable_differs(&self) -> Result<bool> {
        let path = self.config.definition_path(self.platform);
        if !path.is_file() {
            return Ok(false);
        }
        let definition = fs::read(&path).map_err(|source| NativeServiceError::ReadDefinition {
            path: path.clone(),
            source,
        })?;
        self.ensure_owned_definition(&definition)?;
        let text =
            std::str::from_utf8(&definition).map_err(NativeServiceError::DefinitionNotUtf8)?;
        let installed = match self.platform {
            NativeServicePlatform::Macos => launchd_executable(text)?,
            NativeServicePlatform::Linux => systemd_executable(text)?,
        };
        Ok(Path::new(&installed) != self.config.executable)
    }

    pub fn uninstall(&self) -> Result<()> {
        let path = self.config.definition_path(self.platform);
        if !path.exists() {
            return Ok(());
        }
        self.stop(true)?;
        if path.exists() {
            fs::remove_file(&path).map_err(|source| NativeServiceError::RemoveDefinition {
                path: path.clone(),
                source,
            })?;
        }
        if self.platform == NativeServicePlatform::Linux {
            self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
        }
        Ok(())
    }

    /// Neither launchd nor systemd creates a missing log directory; systemd
    /// fails the unit instead. The log can hold paths and error details, so
    /// only the user can read it.
    fn prepare_stderr_log(&self) -> Result<()> {
        let Some(path) = self.config.stderr_path.as_deref() else {
            return Ok(());
        };
        let directory = path.parent().ok_or(NativeServiceError::LogHasNoParent)?;
        fs::create_dir_all(directory).map_err(|source| NativeServiceError::CreateLogDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| NativeServiceError::CreateLog {
                path: path.to_path_buf(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(
                |source| NativeServiceError::RestrictLogDirectory {
                    path: directory.to_path_buf(),
                    source,
                },
            )?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| NativeServiceError::RestrictLog {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        #[cfg(not(unix))]
        drop(file);
        Ok(())
    }

    fn require_installed(&self) -> Result<()> {
        let path = self.config.definition_path(self.platform);
        if !path.is_file() {
            return Err(NativeServiceError::NotInstalled { path });
        }
        let installed = fs::read(&path).map_err(|source| NativeServiceError::ReadDefinition {
            path: path.clone(),
            source,
        })?;
        self.ensure_owned_definition(&installed)?;
        Ok(())
    }

    fn ensure_owned_definition(&self, definition: &[u8]) -> Result<()> {
        let installed_home = normalize_absolute(&installed_home(self.platform, definition)?)?;
        let requested_home = normalize_absolute(&self.config.home)?;
        if installed_home != requested_home {
            return Err(NativeServiceError::HomeMismatch {
                installed: installed_home,
                requested: requested_home,
            });
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
    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous service-manager probe run off the runtime; launchd and systemd offer no unload notification"
    )]
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
                        _ => {
                            return Err(NativeServiceError::SystemctlNotInactive {
                                detail: output_detail(&output),
                            })
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(NativeServiceError::UnloadNotConfirmed)
    }

    fn run_checked(&self, program: &str, args: &[OsString]) -> Result<()> {
        let output = self.runner.run(OsStr::new(program), args)?;
        if !output.success {
            return Err(NativeServiceError::CommandFailed {
                program: program.to_owned(),
                detail: output_detail(&output),
            });
        }
        Ok(())
    }

    fn current_uid(&self) -> Result<u32> {
        let output = self.runner.run(OsStr::new("/usr/bin/id"), &["-u".into()])?;
        if !output.success {
            return Err(NativeServiceError::CurrentUidUnavailable {
                detail: output_detail(&output),
            });
        }
        output
            .stdout
            .trim()
            .parse()
            .map_err(NativeServiceError::ParseUid)
    }

    fn background_item(&self) -> BackgroundItemStatus {
        self.runner
            .background_item_status(&self.config.definition_path(self.platform))
    }

    fn set_macos_enabled(&self, enabled: bool) -> Result<()> {
        let target = format!("gui/{}/{}", self.current_uid()?, SERVICE_LABEL);
        let verb = if enabled { "enable" } else { "disable" };
        self.run_checked("launchctl", &[verb.into(), target.into()])
    }

    fn run_allow_failure(&self, program: &str, args: &[OsString]) -> Result<CommandOutput> {
        self.runner
            .run(OsStr::new(program), args)
            .map_err(|source| NativeServiceError::ServiceManagerUnavailable {
                message: if self.platform == NativeServicePlatform::Linux {
                    "systemd user services are unavailable; a user systemd session is required (Gents does not change linger or root settings)"
                } else {
                    "launchd user services are unavailable"
                },
                source: Box::new(source),
            })
    }
}

fn validate_config(config: &NativeServiceConfig) -> Result<()> {
    if !config.executable.is_absolute() {
        return Err(NativeServiceError::ExecutableNotAbsolute);
    }
    if !config.home.is_absolute() {
        return Err(NativeServiceError::HomeNotAbsolute);
    }
    if !config.home.is_dir() {
        return Err(NativeServiceError::HomeMissing {
            path: config.home.clone(),
        });
    }
    if !config.executable.is_file() {
        return Err(NativeServiceError::ExecutableMissing {
            path: config.executable.clone(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if fs::metadata(&config.executable)?.permissions().mode() & 0o111 == 0 {
            return Err(NativeServiceError::ExecutableNotExecutable {
                path: config.executable.clone(),
            });
        }
    }
    Ok(())
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(NativeServiceError::PathNotAbsolute);
    }
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or(NativeServiceError::NoExistingAncestor)?;
        missing.push(name.to_owned());
        existing = existing
            .parent()
            .ok_or(NativeServiceError::NoExistingAncestor)?;
    }
    let mut normalized =
        fs::canonicalize(existing).map_err(|source| NativeServiceError::CanonicalizeAncestor {
            path: existing.to_path_buf(),
            source,
        })?;
    for component in missing.into_iter().rev() {
        if component == OsStr::new(".") || component == OsStr::new("..") {
            return Err(NativeServiceError::UnresolvedTraversal);
        }
        normalized.push(component);
    }
    Ok(normalized)
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), AtomicWriteError> {
    let parent = path.parent().ok_or(AtomicWriteError::NoParent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write as _;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// The desktop depends on timely responses from this local runtime. Apple's
/// Adaptive classification follows XPC activity, while the desktop bridge uses
/// loopback HTTP, so it cannot reflect active requests; keep this service in
/// the Interactive class intended for jobs whose responsiveness is user-facing.
fn render_launchd(config: &NativeServiceConfig) -> Result<String> {
    let executable = xml_path(&config.executable, "executable")?;
    let home = xml_path(&config.home, "home")?;
    let search_path = config
        .search_path
        .as_ref()
        .map(|value| -> Result<String> {
            let value = value
                .to_str()
                .ok_or(NativeServiceError::SearchPathNotUtf8)?;
            Ok(format!(
                "<key>PATH</key><string>{}</string>",
                xml_escape(value)?
            ))
        })
        .transpose()?
        .unwrap_or_default();
    let environment = format!("\n  <key>EnvironmentVariables</key>\n  <dict><key>GENTS_SYSTEM_LOG</key><string>1</string>{search_path}</dict>");
    let stderr = config
        .stderr_path
        .as_deref()
        .map(|path| -> Result<String> {
            Ok(format!(
                "\n  <key>StandardErrorPath</key><string>{}</string>",
                xml_path(path, "stderr log")?
            ))
        })
        .transpose()?
        .unwrap_or_default();
    let associated_bundle = config
        .associated_bundle_id
        .as_deref()
        .map(|bundle_id| -> Result<String> {
            Ok(format!(
                "\n  <key>AssociatedBundleIdentifiers</key><array><string>{}</string></array>",
                xml_escape(bundle_id)?
            ))
        })
        .transpose()?
        .unwrap_or_default();
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{SERVICE_LABEL}</string>
  <key>ProgramArguments</key><array>
    <string>{executable}</string><string>server</string><string>--home</string><string>{home}</string>
  </array>{environment}{associated_bundle}{stderr}
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#
    ))
}

fn render_systemd(config: &NativeServiceConfig) -> Result<String> {
    let executable = systemd_arg(
        config
            .executable
            .to_str()
            .ok_or(NativeServiceError::ExecutablePathNotUtf8)?,
    )?;
    let home = systemd_arg(
        config
            .home
            .to_str()
            .ok_or(NativeServiceError::HomePathNotUtf8)?,
    )?;
    let search_path = config
        .search_path
        .as_ref()
        .map(|value| systemd_environment("PATH", &value.to_string_lossy()))
        .transpose()?
        .map(|value| format!("Environment={value}\n"))
        .unwrap_or_default();
    let stderr = config
        .stderr_path
        .as_deref()
        .map(systemd_append_target)
        .transpose()?
        .unwrap_or_else(|| "journal".to_string());
    Ok(format!("[Unit]\nDescription=Gents node runtime\n\n[Service]\nType=simple\nExecStart={executable} \"server\" \"--home\" {home}\nEnvironment=\"GENTS_SYSTEM_LOG=1\"\n{search_path}StandardOutput=journal\nStandardError={stderr}\nRestart=on-failure\nRestartPreventExitStatus={INCOMPATIBLE_STORE_EXIT_CODE} {INSECURE_KEY_EXIT_CODE} {FOREIGN_STORE_EXIT_CODE}\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n"))
}

/// `append:` takes the rest of the line as the path after specifier expansion.
fn systemd_append_target(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or(NativeServiceError::StderrLogPathNotUtf8)?;
    if path
        .chars()
        .any(|ch| ch == '\n' || ch == '\r' || ch == '\0')
    {
        return Err(NativeServiceError::StderrLogControlCharacter);
    }
    Ok(format!("append:{}", path.replace('%', "%%")))
}

fn xml_path(path: &Path, name: &'static str) -> Result<String> {
    let value = path
        .to_str()
        .ok_or(NativeServiceError::PathNotUtf8 { name })?;
    xml_escape(value)
}

fn xml_escape(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|ch| matches!(ch, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}'))
    {
        return Err(NativeServiceError::XmlForbiddenCharacter);
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
        return Err(NativeServiceError::ServiceArgumentControlCharacter);
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
    let text = std::str::from_utf8(definition).map_err(NativeServiceError::DefinitionNotUtf8)?;
    let home = match platform {
        NativeServicePlatform::Macos => launchd_home(text)?,
        NativeServicePlatform::Linux => systemd_home(text)?,
    };
    Ok(PathBuf::from(home))
}

fn launchd_home(plist: &str) -> Result<String> {
    launchd_arguments(plist).map(|mut values| values.swap_remove(3))
}

fn launchd_executable(plist: &str) -> Result<String> {
    launchd_arguments(plist).map(|mut values| values.swap_remove(0))
}

fn launchd_arguments(plist: &str) -> Result<Vec<String>> {
    let value = plist::Value::from_reader_xml(std::io::Cursor::new(plist.as_bytes()))
        .map_err(NativeServiceError::ParseLaunchdPlist)?;
    let dictionary = value
        .as_dictionary()
        .ok_or(NativeServiceError::LaunchdRootNotDictionary)?;
    if dictionary.get("Label").and_then(plist::Value::as_string) != Some(SERVICE_LABEL) {
        return Err(NativeServiceError::LaunchdLabelMismatch);
    }
    let arguments = dictionary
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .ok_or(NativeServiceError::LaunchdNoProgramArguments)?;
    let values = arguments
        .iter()
        .map(|value| {
            value
                .as_string()
                .ok_or(NativeServiceError::LaunchdArgumentNotString)
        })
        .collect::<Result<Vec<_>>>()?;
    if values.get(1).copied() != Some("server")
        || values.get(2).copied() != Some("--home")
        || values.len() != 4
    {
        return Err(NativeServiceError::LaunchdNotForegroundServer);
    }
    Ok(values.into_iter().map(str::to_owned).collect())
}

fn systemd_home(unit: &str) -> Result<String> {
    let exec = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .ok_or(NativeServiceError::SystemdNoExecStart)?;
    let marker = " \"server\" \"--home\" ";
    let (_, home) = exec
        .split_once(marker)
        .ok_or(NativeServiceError::SystemdNotForegroundServer)?;
    parse_systemd_quoted(home)
}

fn systemd_executable(unit: &str) -> Result<String> {
    let exec = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .ok_or(NativeServiceError::SystemdNoExecStart)?;
    let (executable, _) = exec
        .split_once(" \"server\" \"--home\" ")
        .ok_or(NativeServiceError::SystemdNotForegroundServer)?;
    parse_systemd_quoted(executable)
}

fn parse_systemd_quoted(value: &str) -> Result<String> {
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(NativeServiceError::SystemdArgumentNotQuoted)?;
    let mut decoded = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars
                .next()
                .ok_or(NativeServiceError::SystemdTrailingEscape)?
            {
                '\\' => decoded.push('\\'),
                '"' => decoded.push('"'),
                _ => return Err(NativeServiceError::SystemdUnsupportedEscape),
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
                return Err(NativeServiceError::SystemdUnescapedCharacter)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceExit {
    pub reason: String,
    pub restarts: u64,
    pub clean: bool,
    pub code: Option<i32>,
}

impl ServiceExit {
    pub fn incompatible_store(&self) -> bool {
        self.refused_store().is_some()
    }

    pub fn refused_store(&self) -> Option<gents::storage_backend::IncompatibleStoreKind> {
        match self.code {
            Some(INCOMPATIBLE_STORE_EXIT_CODE) => {
                Some(gents::storage_backend::IncompatibleStoreKind::UnknownLineage)
            }
            Some(FOREIGN_STORE_EXIT_CODE) => {
                Some(gents::storage_backend::IncompatibleStoreKind::ForeignVersion)
            }
            Some(INSECURE_KEY_EXIT_CODE) => {
                Some(gents::storage_backend::IncompatibleStoreKind::InsecureKey)
            }
            _ => None,
        }
    }
}

fn launchd_exit_failure(output: &str) -> Option<ServiceExit> {
    if launchd_is_running(output) {
        return None;
    }
    let field = |name: &str| {
        output
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    let restarts = field("runs = ")
        .and_then(|runs| runs.parse::<u64>().ok())
        .unwrap_or_default()
        .saturating_sub(1);
    let (reason, clean, status) = if let Some(signal) = field("last terminating signal = ") {
        (format!("terminated by signal {signal}"), false, None)
    } else {
        let code = field("last exit code = ")?;
        if code.starts_with('(') {
            return None;
        }
        let status = code
            .split(':')
            .next()
            .and_then(|status| status.trim().parse::<i32>().ok());
        if status == Some(0) {
            ("exited normally".to_string(), true, status)
        } else {
            (format!("exited with code {code}"), false, status)
        }
    };
    Some(ServiceExit {
        reason,
        restarts,
        clean,
        code: status,
    })
}

fn systemd_exit_failure(output: &str) -> Option<ServiceExit> {
    let field = |name: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(name)?.strip_prefix('='))
            .unwrap_or_default()
    };
    let (active, sub, result) = (field("ActiveState"), field("SubState"), field("Result"));
    if active == "active" && sub != "auto-restart" {
        return None;
    }
    if sub != "auto-restart" && (result.is_empty() || result == "success") {
        return None;
    }
    Some(ServiceExit {
        reason: format!("{result} (exit status {})", field("ExecMainStatus")),
        restarts: field("NRestarts").parse().unwrap_or_default(),
        clean: false,
        code: (result == "exit-code")
            .then(|| field("ExecMainStatus").parse().ok())
            .flatten(),
    })
}

/// launchd answers `bootstrap` of a background item that Background Task
/// Management withholds with error 5 (EIO). An already loaded job fails the
/// same way, so callers also require the job to be absent.
fn launchd_refused_bootstrap(output: &CommandOutput) -> bool {
    !output.success
        && [&output.stderr, &output.stdout]
            .iter()
            .any(|text| text.contains("Bootstrap failed: 5:"))
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
            value => {
                return Err(NativeServiceError::LaunchctlUnknownOverride {
                    value: value.to_owned(),
                })
            }
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
        Err(NativeServiceError::LaunchctlInspectFailed {
            detail: output_detail(output),
        })
    }
}
