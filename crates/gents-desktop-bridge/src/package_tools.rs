//! Tool lookup inside a packaged build.
//!
//! An AppImage launcher prepends the mounted `usr/bin` to `PATH` and search
//! lists, so the package's tools shadow the host's. Those copies come from the
//! distribution the package was built on and can be silently wrong on a newer
//! host (e.g. Debian `xdg-open` matches Plasma 4/5 only: on Plasma 6 it opens
//! nothing yet exits 0). Package entries keep their place on `PATH` — a host
//! without `xdg-open` needs a fallback — but stop coming first.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

// A host program must not search these: it would load the package's libraries,
// plugins and data in place of its own.
const PACKAGE_SEARCH_LISTS: &[&str] = &[
    "PATH",
    "LD_LIBRARY_PATH",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "PYTHONPATH",
    "PERLLIB",
    "PERL5LIB",
    "QT_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GTK_PATH",
    "GSETTINGS_SCHEMA_DIR",
    "GIO_EXTRA_MODULES",
];

const PACKAGE_LOCATIONS: &[&str] = &[
    "GTK_EXE_PREFIX",
    "GTK_DATA_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "LD_PRELOAD",
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "OWD",
];

// The launcher forces these; the original is not recoverable, so a host program
// picks the session's own display backend and theme rather than the package's.
const LAUNCHER_OVERRIDES: &[&str] = &["GDK_BACKEND", "GTK_THEME", "APPIMAGE_GTK_THEME"];

pub fn prefer_host_tools() {
    let Some(reordered) = host_first_search_path(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("APPDIR").as_deref().map(Path::new),
        std::env::var_os("APPIMAGE").as_deref().map(Path::new),
    ) else {
        return;
    };
    tracing::info!("host tools now resolve ahead of the ones inside the application package");
    std::env::set_var("PATH", reordered);
}

/// Give `command` the environment a program would have had if it were not
/// launched from inside this package. Only for programs that are not ours:
/// this package's own helpers are built against the libraries it carries and
/// must keep the launcher's environment.
pub fn prepare_host_command(command: &mut Command) {
    for (name, value) in host_environment_changes(
        |name| std::env::var_os(name),
        std::env::var_os("APPDIR").as_deref().map(Path::new),
        std::env::var_os("APPIMAGE").as_deref().map(Path::new),
    ) {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
}

fn host_environment_changes(
    read: impl Fn(&str) -> Option<OsString>,
    app_dir: Option<&Path>,
    app_image: Option<&Path>,
) -> Vec<(&'static str, Option<OsString>)> {
    if app_image.is_none() {
        return Vec::new();
    }
    let packaged = |path: &Path| {
        gents_server::native_service::inside_temporary_package(path, app_dir, app_image)
    };
    let mut changes = Vec::new();
    for name in PACKAGE_SEARCH_LISTS {
        let Some(value) = read(name) else {
            continue;
        };
        let kept: Vec<PathBuf> = std::env::split_paths(&value)
            .filter(|entry| !packaged(entry))
            .collect();
        if kept.len() == std::env::split_paths(&value).count() {
            continue;
        }
        // A list emptied of the package's entries is removed, never set to
        // "": an empty entry reads as the working directory to most loaders.
        changes.push((
            *name,
            std::env::join_paths(kept).ok().filter(|v| !v.is_empty()),
        ));
    }
    for name in PACKAGE_LOCATIONS {
        if matches!(*name, "APPDIR" | "APPIMAGE" | "ARGV0" | "OWD") {
            if read(name).is_some() {
                changes.push((*name, None));
            }
        } else if read(name).is_some_and(|value| packaged(Path::new(&value))) {
            changes.push((*name, None));
        }
    }
    for name in LAUNCHER_OVERRIDES {
        if read(name).is_some() {
            changes.push((*name, None));
        }
    }
    changes
}

fn host_first_search_path(
    search: Option<&OsStr>,
    app_dir: Option<&Path>,
    app_image: Option<&Path>,
) -> Option<OsString> {
    let entries: Vec<PathBuf> = std::env::split_paths(search?).collect();
    let (packaged, host): (Vec<PathBuf>, Vec<PathBuf>) =
        entries.iter().cloned().partition(|entry| {
            gents_server::native_service::inside_temporary_package(entry, app_dir, app_image)
        });
    if packaged.is_empty() {
        return None;
    }
    let reordered: Vec<PathBuf> = host.into_iter().chain(packaged).collect();
    if reordered == entries {
        return None;
    }
    std::env::join_paths(reordered).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_image() -> Option<&'static Path> {
        Some(Path::new("/home/user/.local/bin/Gents.AppImage"))
    }

    fn mount() -> Option<&'static Path> {
        Some(Path::new("/tmp/.mount_gents123"))
    }

    /// A released AppImage's launch environment, read from a running build on
    /// a Plasma 6 Wayland session.
    fn measured_launch_environment() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "PATH",
                "/tmp/.mount_gents123/usr/bin/:/usr/local/bin:/usr/bin",
            ),
            (
                "LD_LIBRARY_PATH",
                "/tmp/.mount_gents123/usr/lib/:/tmp/.mount_gents123/usr/lib/x86_64-linux-gnu/",
            ),
            (
                "XDG_DATA_DIRS",
                "/tmp/.mount_gents123/usr/share/:/usr/share:/usr/local/share",
            ),
            ("GDK_BACKEND", "x11"),
            ("GTK_THEME", "Adwaita:light"),
            ("GTK_EXE_PREFIX", "/tmp/.mount_gents123//usr"),
            (
                "GDK_PIXBUF_MODULE_FILE",
                "/tmp/.mount_gents123//usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache",
            ),
            (
                "QT_PLUGIN_PATH",
                "/tmp/.mount_gents123/usr/lib/qt4/plugins/",
            ),
            ("APPDIR", "/tmp/.mount_gents123"),
            ("APPIMAGE", "/home/user/.local/bin/Gents.AppImage"),
            ("HOME", "/home/user"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ]
    }

    fn changes_for(
        environment: &[(&'static str, &'static str)],
    ) -> std::collections::BTreeMap<&'static str, Option<String>> {
        host_environment_changes(
            |name| {
                environment
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(*value))
            },
            mount(),
            app_image(),
        )
        .into_iter()
        .map(|(name, value)| (name, value.map(|v| v.to_string_lossy().into_owned())))
        .collect()
    }

    #[test]
    fn a_host_program_loses_every_one_of_the_packages_contributions() {
        let changes = changes_for(&measured_launch_environment());

        assert_eq!(
            changes["LD_LIBRARY_PATH"], None,
            "wholly inside the package"
        );
        assert_eq!(changes["QT_PLUGIN_PATH"], None);
        assert_eq!(changes["GTK_EXE_PREFIX"], None);
        assert_eq!(changes["GDK_PIXBUF_MODULE_FILE"], None);
        assert_eq!(changes["PATH"].as_deref(), Some("/usr/local/bin:/usr/bin"));
        assert_eq!(
            changes["XDG_DATA_DIRS"].as_deref(),
            Some("/usr/share:/usr/local/share"),
            "handler lookup must not search the package's applications first"
        );
        assert_eq!(
            changes["GDK_BACKEND"], None,
            "Wayland sessions keep Wayland"
        );
        assert_eq!(changes["GTK_THEME"], None);
        assert_eq!(changes["APPDIR"], None);
        assert_eq!(changes["APPIMAGE"], None);
        assert!(!changes.contains_key("HOME"));
        assert!(!changes.contains_key("WAYLAND_DISPLAY"));
    }

    #[test]
    fn an_ordinary_install_spawns_with_the_environment_it_has() {
        let changes = host_environment_changes(
            |name| {
                measured_launch_environment()
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(*value))
            },
            None,
            None,
        );
        assert!(
            changes.is_empty(),
            "no launcher means nothing to undo: {changes:?}"
        );
    }
}
