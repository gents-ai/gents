use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use gents_eval::authoring::write::WriteHost;
use gents_store::identity::NodeIdentity;

pub(crate) struct CliWriteHost(pub(crate) PathBuf);

#[async_trait::async_trait]
impl WriteHost for CliWriteHost {
    fn overwritable_home(
        &self,
        home_dir: &Path,
        user_home: Option<&Path>,
    ) -> Result<Option<PathBuf>> {
        crate::overwritable_home(home_dir, user_home)
    }

    fn is_gents_home(&self, dir: &Path) -> Result<bool> {
        Ok(gents_service::home_state::read_init_config(dir)?.is_some())
    }

    async fn caller_identity(&self) -> Result<Arc<dyn NodeIdentity>> {
        Ok(Arc::new(
            gents_service::home_state::client_identity_for(&self.0).await?,
        ))
    }
}
