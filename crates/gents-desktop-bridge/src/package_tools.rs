//! Tool lookup inside a packaged build.
//!
//! An AppImage's `AppRun` prepends the mounted `usr/bin` to `PATH`, so the
//! tools the package carries shadow the host's own. Those copies come from the
//! distribution the package was built on, and an older copy can be silently
//! wrong on a newer desktop rather than absent: the `xdg-open` shipped by
//! Debian's `xdg-utils` matches `KDE_SESSION_VERSION` 4 and 5 only, so on
//! Plasma 6 its `open_kde` falls through, opens nothing, and still exits 0.
//! A caller asking for a browser is told it succeeded, and a sign-in that was
//! waiting on the resulting callback waits for something that cannot arrive.
//!
//! Dropping those entries would leave a host without `xdg-open` no fallback,
//! so they keep their place on `PATH` and only stop coming first.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Search lists an AppImage launcher rewrites so the application finds its own
/// copies first. A host program must not search them at all: it would load
/// this package's libraries, plugins and data in place of its own.
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

/// Single values that name a location inside the package.
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

/// Values the launcher forces for the application's own sake. The original is
/// not recoverable, so a host program is better off with none: it then picks
/// the session's own display backend and theme rather than this package's.
const LAUNCHER_OVERRIDES: &[&str] = &["GDK_BACKEND", "GTK_THEME", "APPIMAGE_GTK_THEME"];

/// Put the host's tools ahead of a temporary package mount's copies. Called
/// once during startup, before anything spawns, so every child process the
/// application launches inherits the order.
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
/// launched from inside this package: no package library paths, no package
/// data directories, no forced display backend.
///
/// Only for programs that are not ours. This package's own helpers are built
/// against the libraries it carries and must keep the launcher's environment.
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

/// What a host program's environment needs changed. `Some` replaces the value,
/// `None` removes it. Empty when this is not a packaged launch, so an ordinary
/// install spawns with the environment it already has.
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

/// `search` with every entry inside the package mount moved after the host's,
/// each group keeping its own order. `None` when there is nothing to move, so
/// an ordinary install never rewrites its own environment.
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

    /// The environment a released AppImage hands its process, read from a
    /// running 0.18.5 build on a Plasma 6 Wayland session.
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

        // Libraries and plugins: the host program must load its own.
        assert_eq!(
            changes["LD_LIBRARY_PATH"], None,
            "wholly inside the package"
        );
        assert_eq!(changes["QT_PLUGIN_PATH"], None);
        assert_eq!(changes["GTK_EXE_PREFIX"], None);
        assert_eq!(changes["GDK_PIXBUF_MODULE_FILE"], None);
        // Mixed lists keep the host's entries, in order.
        assert_eq!(changes["PATH"].as_deref(), Some("/usr/local/bin:/usr/bin"));
        assert_eq!(
            changes["XDG_DATA_DIRS"].as_deref(),
            Some("/usr/share:/usr/local/share"),
            "handler lookup must not search the package's applications first"
        );
        // Forced for the application: a browser picks the session's own.
        assert_eq!(
            changes["GDK_BACKEND"], None,
            "Wayland sessions keep Wayland"
        );
        assert_eq!(changes["GTK_THEME"], None);
        // The program is not running inside a package.
        assert_eq!(changes["APPDIR"], None);
        assert_eq!(changes["APPIMAGE"], None);
        // Untouched: nothing the package did not contribute.
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

    #[test]
    fn a_packaged_launch_keeps_its_tools_but_stops_preferring_them() {
        // The order AppRun hands the process.
        let launched = OsString::from(
            "/tmp/.mount_gents123/usr/bin/:/tmp/.mount_gents123/usr/sbin/:/usr/local/bin:/usr/bin",
        );
        assert_eq!(
            host_first_search_path(Some(&launched), mount(), app_image()),
            Some(OsString::from(
                "/usr/local/bin:/usr/bin:/tmp/.mount_gents123/usr/bin/:/tmp/.mount_gents123/usr/sbin/"
            )),
            "the package's own tools remain reachable, behind the host's"
        );
    }

    #[test]
    fn an_ordinary_install_is_left_alone() {
        let path = OsString::from("/usr/local/bin:/usr/bin");
        // No launcher: a .deb, a macOS bundle, a developer build.
        assert_eq!(host_first_search_path(Some(&path), None, None), None);
        // An extracted AppRun exports APPDIR and no APPIMAGE.
        assert_eq!(
            host_first_search_path(
                Some(&OsString::from("/opt/gents/squashfs-root/usr/bin:/usr/bin")),
                Some(Path::new("/opt/gents/squashfs-root")),
                None
            ),
            None
        );
        assert_eq!(host_first_search_path(None, mount(), app_image()), None);
    }

    #[test]
    fn an_order_that_already_holds_is_not_rewritten() {
        let ordered = OsString::from("/usr/bin:/tmp/.mount_gents123/usr/bin");
        assert_eq!(
            host_first_search_path(Some(&ordered), mount(), app_image()),
            None
        );
    }

    #[test]
    fn the_hosts_own_order_is_preserved() {
        let launched =
            OsString::from("/tmp/.mount_gents123/usr/bin:/home/user/.cargo/bin:/usr/bin:/bin");
        assert_eq!(
            host_first_search_path(Some(&launched), mount(), app_image()),
            Some(OsString::from(
                "/home/user/.cargo/bin:/usr/bin:/bin:/tmp/.mount_gents123/usr/bin"
            ))
        );
    }

    #[test]
    fn a_mount_is_recognized_by_its_name_when_appdir_is_absent() {
        let launched = OsString::from("/tmp/.mount_gents123/usr/bin:/usr/bin");
        assert_eq!(
            host_first_search_path(Some(&launched), None, app_image()),
            Some(OsString::from("/usr/bin:/tmp/.mount_gents123/usr/bin"))
        );
    }
}
