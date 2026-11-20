use std::path::{Path, PathBuf};

use gents_store::client_journal::paths::DesktopPaths;

/// Throwaway user and desktop homes under a test's own directory for
/// every host and CLI process it spawns. Without them the runtime CLI resolves
/// its client identity, default homes and service definitions from the
/// operator's account, which test builds refuse.
pub struct ProcessHomes {
    root: PathBuf,
}

impl ProcessHomes {
    pub fn under(directory: &Path) -> Self {
        let root = directory.join("process-homes");
        for home in ["user", "desktop"] {
            std::fs::create_dir_all(root.join(home)).expect("creating a throwaway process home");
        }
        Self { root }
    }

    /// The desktop home whose `cli` child holds the CLI client identity that
    /// `init` creates and later CLI commands sign with.
    pub fn cli_identity(&self) -> DesktopPaths {
        DesktopPaths::from_root(self.root.join("desktop").join("cli"))
    }

    pub fn env(&self) -> [(&'static str, PathBuf); 4] {
        let user = self.root.join("user");
        [
            (
                gents_store::client_journal::paths::DESKTOP_HOME_ENV,
                self.root.join("desktop"),
            ),
            ("XDG_DATA_HOME", user.join(".local").join("share")),
            ("XDG_CONFIG_HOME", user.join(".config")),
            ("HOME", user),
        ]
    }
}
