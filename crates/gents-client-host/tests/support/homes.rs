use std::path::{Path, PathBuf};

use gents_desktop_core::client::DesktopPaths;

/// Throwaway user, runtime and desktop homes under a test's own directory for
/// every host and CLI process it spawns. Without them the runtime CLI resolves
/// its client identity, default homes and service definitions from the
/// operator's account, which test builds refuse.
pub struct ProcessHomes {
    root: PathBuf,
}

impl ProcessHomes {
    pub fn under(directory: &Path) -> Self {
        let root = directory.join("process-homes");
        for home in ["user", "runtime", "desktop"] {
            std::fs::create_dir_all(root.join(home)).expect("creating a throwaway process home");
        }
        Self { root }
    }

    /// The desktop home whose `cli` child holds the CLI client identity that
    /// `init` creates and later CLI commands sign with.
    #[allow(
        dead_code,
        reason = "shared by test binaries that do not all sign as the CLI"
    )]
    pub fn cli_identity(&self) -> DesktopPaths {
        DesktopPaths::from_root(self.root.join("desktop").join("cli"))
    }

    pub fn env(&self) -> [(&'static str, PathBuf); 5] {
        let user = self.root.join("user");
        [
            ("GENTS_HOME", self.root.join("runtime")),
            (
                gents_desktop_core::client::DESKTOP_HOME_ENV,
                self.root.join("desktop"),
            ),
            ("XDG_DATA_HOME", user.join(".local").join("share")),
            ("XDG_CONFIG_HOME", user.join(".config")),
            ("HOME", user),
        ]
    }
}
