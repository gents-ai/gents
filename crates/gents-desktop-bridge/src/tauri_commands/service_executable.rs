//! Which executable a native Gents service runs, and where it has to live.
//!
//! A service definition outlives the desktop process that wrote it, so the
//! executable it names must outlive it too. A packaged build launched from a
//! temporary AppImage mount keeps a copy of its runtime under the desktop
//! home and points the service at that copy; every other install points at
//! the executable where it already sits.

use std::io::Read;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, Runtime};

use crate::error::{BridgeError, BridgeErrorCode};

/// Where the runtime copied out of a packaged build is kept, relative to the
/// desktop home.
const RUNTIME_DIR: &str = "runtime";

/// Names a copy still being written, so it is never mistaken for the runtime.
const INCOMING_PREFIX: &str = ".incoming.";

/// How long a staged copy must sit untouched before it counts as abandoned.
const ABANDONED_COPY_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Refresh and start run in one process. One lock keeps them from publishing
/// the same destination at the same time.
static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

static INSTALL_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A resolved runtime executable and what it takes to make a service own it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceExecutable {
    /// The executable already sits at a path that survives the desktop app.
    InPlace(PathBuf),
    /// The executable ships inside a temporary AppImage mount. `installed` is
    /// the copy under the desktop home that the service definition names.
    Packaged { source: PathBuf, installed: PathBuf },
}

impl ServiceExecutable {
    /// The path a service definition points at.
    pub(crate) fn service_path(&self) -> &Path {
        match self {
            Self::InPlace(path) => path,
            Self::Packaged { installed, .. } => installed,
        }
    }

    /// Puts the bytes behind [`Self::service_path`] in place, and reports
    /// whether they changed. Callers do this before a definition is installed
    /// or started, and at startup, never on a status read.
    pub(crate) fn install(&self) -> Result<bool, BridgeError> {
        let Self::Packaged { source, installed } = self else {
            return Ok(false);
        };
        install_packaged_executable(source, installed).map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::Backend,
                format!(
                    "Could not copy the Gents runtime to {} for the background agent: {error}. Check the free space and permissions there, then try again.",
                    installed.display()
                ),
            )
        })
    }
}

/// Resolves the runtime executable a native service should run, preferring an
/// explicit `GENTS_BIN`, then the executable shipped beside the desktop app,
/// then `PATH`.
pub(crate) fn resolve_service_executable<R: Runtime>(
    app: &AppHandle<R>,
    desktop_home: &Path,
) -> Result<ServiceExecutable, BridgeError> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "native Gents services are supported only on macOS and Linux",
        ));
    }
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("GENTS_BIN").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "GENTS_BIN must be an absolute path",
            ));
        }
        let executable = executable_file(&path)
            .then(|| std::fs::canonicalize(path).ok())
            .flatten()
            .ok_or_else(|| {
                BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "GENTS_BIN must name an existing executable file",
                )
            })?;
        return Ok(classify(executable, desktop_home));
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            candidates.push(parent.join(gents_executable_name()));
        }
    }
    if let Ok(resources) = app.path().resource_dir() {
        candidates.push(resources.join(gents_executable_name()));
    }
    if let Some(search) = std::env::var_os("PATH") {
        candidates
            .extend(std::env::split_paths(&search).map(|dir| dir.join(gents_executable_name())));
    }
    let fallback = candidates.first().cloned();
    let executable = candidates
        .into_iter()
        .find(|path| executable_file(path))
        .and_then(|path| std::fs::canonicalize(path).ok())
        .or(fallback)
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "Could not find the Gents runtime executable. Reinstall Gents or set GENTS_BIN to its absolute path.",
            )
        })?;
    Ok(classify(executable, desktop_home))
}

fn classify(executable: PathBuf, desktop_home: &Path) -> ServiceExecutable {
    let app_dir = std::env::var_os("APPDIR").map(PathBuf::from);
    let app_image = std::env::var_os("APPIMAGE").map(PathBuf::from);
    classify_with_package(
        executable,
        desktop_home,
        app_dir.as_deref(),
        app_image.as_deref(),
    )
}

fn classify_with_package(
    executable: PathBuf,
    desktop_home: &Path,
    app_dir: Option<&Path>,
    app_image: Option<&Path>,
) -> ServiceExecutable {
    if gents_server::native_service::inside_temporary_package(&executable, app_dir, app_image) {
        return ServiceExecutable::Packaged {
            installed: desktop_home.join(RUNTIME_DIR).join(gents_executable_name()),
            source: executable,
        };
    }
    ServiceExecutable::InPlace(executable)
}

/// Copies the packaged runtime to its durable path, keyed by content so an
/// upgraded package refreshes it and an unchanged one costs only a read.
fn install_packaged_executable(source: &Path, installed: &Path) -> std::io::Result<bool> {
    let _guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if already_installed(source, installed)? {
        return Ok(false);
    }
    let directory = installed.parent().ok_or_else(|| {
        std::io::Error::other("the service executable path has no parent directory")
    })?;
    std::fs::create_dir_all(directory)?;
    // A running service holds the previous inode open, so new bytes land
    // beside it and are renamed over it. Writing the path in place would fail
    // with ETXTBSY. The attempt number keeps two callers in this process from
    // sharing one staging file: a rename would then publish a short file.
    let attempt = INSTALL_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let incoming = directory.join(format!("{INCOMING_PREFIX}{}.{attempt}", std::process::id()));
    discard_abandoned_copies(directory);
    let staged = stage(source, &incoming);
    if staged.is_err() {
        let _ = std::fs::remove_file(&incoming);
    }
    staged?;
    if let Err(error) = std::fs::rename(&incoming, installed) {
        let _ = std::fs::remove_file(&incoming);
        return Err(error);
    }
    Ok(true)
}

/// Whether the durable copy already holds the packaged bytes. Comparing the
/// copy itself, rather than a record of what was written, stays true whichever
/// build wrote it last and whatever interrupted the write.
fn already_installed(source: &Path, installed: &Path) -> std::io::Result<bool> {
    // A symlink is not the copy. Following it would treat the link target as
    // installed and let the service definition record that target.
    if !regular_executable(installed) {
        return Ok(false);
    }
    Ok(content_digest(installed).ok() == Some(content_digest(source)?))
}

fn regular_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Removes copies a killed process left mid-write, so a crash costs one
/// abandoned file rather than one per crash. The age bound keeps this clear of
/// a copy another process is writing right now.
fn discard_abandoned_copies(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(INCOMING_PREFIX)
        {
            continue;
        }
        let abandoned = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| {
                std::time::SystemTime::now()
                    .duration_since(modified)
                    .map_err(std::io::Error::other)
            })
            .is_ok_and(|age| age > ABANDONED_COPY_AGE);
        if abandoned {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn stage(source: &Path, incoming: &Path) -> std::io::Result<()> {
    // `create_new` fails if the name exists, including when it is a symlink,
    // so the write cannot be redirected at a path someone else planted.
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(incoming)?;
    let copied = (|| {
        let mut input = std::fs::File::open(source)?;
        std::io::copy(&mut input, &mut output)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        // Durable before the rename, so a crash cannot publish a short file.
        output.sync_all()
    })();
    if copied.is_err() {
        drop(output);
        let _ = std::fs::remove_file(incoming);
    }
    copied
}

/// Streams the file so a large packaged runtime costs one buffer, not its size.
fn content_digest(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn gents_executable_name() -> &'static str {
    if cfg!(windows) {
        "gents.exe"
    } else {
        "gents"
    }
}

pub(crate) fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_home() -> &'static Path {
        Path::new("/home/user/.local/share/gents/desktop")
    }

    #[test]
    fn an_installed_executable_is_owned_where_it_sits() {
        assert_eq!(
            classify_with_package(PathBuf::from("/usr/bin/gents"), desktop_home(), None, None),
            ServiceExecutable::InPlace(PathBuf::from("/usr/bin/gents"))
        );
        // Packaging outside AppImage, macOS bundles included, exports neither
        // variable and keeps owning its executable in place.
        let bundled = PathBuf::from("/Applications/Gents.app/Contents/MacOS/gents");
        assert_eq!(
            classify_with_package(bundled.clone(), desktop_home(), None, None),
            ServiceExecutable::InPlace(bundled)
        );
        // An extracted AppRun exports APPDIR without APPIMAGE, and that
        // directory is as durable as the user left it.
        assert_eq!(
            classify_with_package(
                PathBuf::from("/opt/gents/squashfs-root/usr/bin/gents"),
                desktop_home(),
                Some(Path::new("/opt/gents/squashfs-root")),
                None,
            ),
            ServiceExecutable::InPlace(PathBuf::from("/opt/gents/squashfs-root/usr/bin/gents"))
        );
    }

    #[test]
    fn a_mounted_appimage_runtime_is_owned_under_the_desktop_home() {
        let installed = desktop_home().join("runtime").join("gents");
        let mounted = PathBuf::from("/tmp/.mount_gents123/usr/bin/gents");
        for app_dir in [Some(Path::new("/tmp/.mount_gents123")), None] {
            assert_eq!(
                classify_with_package(
                    mounted.clone(),
                    desktop_home(),
                    app_dir,
                    Some(Path::new("/home/user/.local/bin/Gents.AppImage")),
                ),
                ServiceExecutable::Packaged {
                    source: mounted.clone(),
                    installed: installed.clone(),
                }
            );
        }
        // A runtime outside the mount stays where it is even while an
        // AppImage launcher is active.
        assert_eq!(
            classify_with_package(
                PathBuf::from("/usr/bin/gents"),
                desktop_home(),
                Some(Path::new("/tmp/.mount_gents123")),
                Some(Path::new("/home/user/.local/bin/Gents.AppImage")),
            ),
            ServiceExecutable::InPlace(PathBuf::from("/usr/bin/gents"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_packaged_runtime_is_copied_once_and_refreshed_when_the_package_changes() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("temporary roots");
        let source = temp.path().join("mount/usr/bin/gents");
        std::fs::create_dir_all(source.parent().expect("mount directory")).expect("mount");
        std::fs::write(&source, b"packaged runtime").expect("packaged runtime");
        let executable = ServiceExecutable::Packaged {
            installed: temp.path().join("desktop").join(RUNTIME_DIR).join("gents"),
            source: source.clone(),
        };
        let installed = executable.service_path().to_path_buf();

        executable.install().expect("first install");
        assert_eq!(
            std::fs::read(&installed).expect("installed runtime"),
            b"packaged runtime"
        );
        assert!(executable_file(&installed));
        let first = std::fs::metadata(&installed)
            .expect("installed metadata")
            .ino();

        executable.install().expect("unchanged install");
        assert_eq!(
            std::fs::metadata(&installed)
                .expect("installed metadata")
                .ino(),
            first,
            "an unchanged package must not be copied again"
        );

        std::fs::write(&source, b"upgraded runtime").expect("upgraded runtime");
        executable.install().expect("upgrade install");
        assert_eq!(
            std::fs::read(&installed).expect("installed runtime"),
            b"upgraded runtime"
        );
        assert!(
            std::fs::read_dir(installed.parent().expect("runtime directory"))
                .expect("runtime directory")
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(INCOMING_PREFIX)),
            "a completed install leaves no staged copy behind"
        );
    }

    #[test]
    fn an_abandoned_copy_is_discarded_and_a_live_one_is_left_alone() {
        let temp = tempfile::tempdir().expect("temporary roots");
        let abandoned = temp.path().join(format!("{INCOMING_PREFIX}1"));
        let live = temp.path().join(format!("{INCOMING_PREFIX}2"));
        let runtime = temp.path().join("gents");
        for path in [&abandoned, &live, &runtime] {
            std::fs::write(path, b"bytes").expect("staged copy");
        }
        let stale = std::time::SystemTime::now() - ABANDONED_COPY_AGE - ABANDONED_COPY_AGE;
        std::fs::File::options()
            .write(true)
            .open(&abandoned)
            .expect("staged copy")
            .set_times(std::fs::FileTimes::new().set_modified(stale))
            .expect("aged staged copy");

        discard_abandoned_copies(temp.path());

        assert!(!abandoned.exists());
        assert!(
            live.exists(),
            "a copy another process may still be writing must be left alone"
        );
        assert!(runtime.exists(), "the runtime itself is never swept");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_installs_publish_a_complete_runtime() {
        let temp = tempfile::tempdir().expect("temporary roots");
        let source = temp.path().join("mount/usr/bin/gents");
        std::fs::create_dir_all(source.parent().expect("mount directory")).expect("mount");
        std::fs::write(&source, b"packaged runtime").expect("packaged runtime");
        let executable = ServiceExecutable::Packaged {
            installed: temp.path().join("desktop").join(RUNTIME_DIR).join("gents"),
            source,
        };
        let installed = executable.service_path().to_path_buf();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    executable.install().expect("install");
                });
            }
        });
        assert_eq!(
            std::fs::read(&installed).expect("installed runtime"),
            b"packaged runtime"
        );
        assert!(regular_executable(&installed));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_runtime_path_is_replaced_with_the_packaged_bytes() {
        let temp = tempfile::tempdir().expect("temporary roots");
        let source = temp.path().join("mount/usr/bin/gents");
        std::fs::create_dir_all(source.parent().expect("mount directory")).expect("mount");
        std::fs::write(&source, b"packaged runtime").expect("packaged runtime");
        let elsewhere = temp.path().join("elsewhere");
        std::fs::write(&elsewhere, b"not the runtime").expect("link target");
        let executable = ServiceExecutable::Packaged {
            installed: temp.path().join("desktop").join(RUNTIME_DIR).join("gents"),
            source,
        };
        let installed = executable.service_path().to_path_buf();
        std::fs::create_dir_all(installed.parent().expect("runtime directory")).expect("runtime");
        std::os::unix::fs::symlink(&elsewhere, &installed).expect("symlink");

        executable.install().expect("replace symlink");

        assert!(std::fs::symlink_metadata(&installed)
            .expect("installed metadata")
            .file_type()
            .is_file());
        assert_eq!(
            std::fs::read(&installed).expect("installed runtime"),
            b"packaged runtime"
        );
        assert_eq!(
            std::fs::read(&elsewhere).expect("link target"),
            b"not the runtime"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stage_does_not_follow_a_symlink() {
        let temp = tempfile::tempdir().expect("temporary roots");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        let incoming = temp.path().join(".incoming.planted");
        std::fs::write(&source, b"packaged runtime").expect("source");
        std::fs::write(&target, b"original").expect("target");
        std::os::unix::fs::symlink(&target, &incoming).expect("symlink");

        assert!(stage(&source, &incoming).is_err());
        assert_eq!(std::fs::read(&target).expect("target"), b"original");
    }

    #[test]
    fn a_missing_package_fails_with_the_path_it_could_not_write() {
        let temp = tempfile::tempdir().expect("temporary roots");
        let installed = temp.path().join("desktop/runtime/gents");
        let error = ServiceExecutable::Packaged {
            source: temp.path().join("absent/gents"),
            installed: installed.clone(),
        }
        .install()
        .expect_err("a missing packaged runtime cannot be installed");
        assert_eq!(error.code, BridgeErrorCode::Backend);
        assert!(error.message.contains(&installed.display().to_string()));
        assert!(!installed.exists());
    }

    #[test]
    fn in_place_executables_need_no_install_step() {
        assert!(ServiceExecutable::InPlace(PathBuf::from("/usr/bin/gents"))
            .install()
            .is_ok());
    }
}
