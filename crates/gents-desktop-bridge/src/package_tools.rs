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
