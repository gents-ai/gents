use std::sync::Arc;

use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use tempfile::TempDir;

pub(crate) async fn boot_core() -> (Arc<ClientCore>, TempDir) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core starts");
    (Arc::new(core), tempdir)
}
