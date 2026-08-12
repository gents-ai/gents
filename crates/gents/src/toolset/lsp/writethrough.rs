use std::path::Path;

use super::catalog::primary_for_file;
use super::pool::{LspPool, PoolKey};
use super::LspToolConfig;

/// File-tool hook that talks only to already-Ready pooled clients.
#[derive(Clone)]
pub struct LspWritethrough {
    pool: LspPool,
    config: LspToolConfig,
}

impl LspWritethrough {
    pub fn new(pool: LspPool, config: LspToolConfig) -> Self {
        Self { pool, config }
    }

    /// Notify an already-Ready client. Never starts a server. Never invokes
    /// Biome/SwiftLint single-shot adapters.
    pub async fn after_mutation(&self, path: &Path) -> Option<String> {
        let server = primary_for_file(&self.config.servers, path)?;
        if server.is_linter {
            return None;
        }
        let key = PoolKey {
            session_id: self.config.session_id.clone(),
            behavior_id: self.config.behavior_id.clone(),
            workspace_root: self.config.workspace.clone(),
            server_name: server.name.clone(),
            config_digest: self.config.digest.clone(),
        };
        let lease = self.pool.get_ready(&key).await?;
        let uri = format!("file://{}", path.display());
        let version = lease.client().tracked_version(&uri).await.unwrap_or(1) + 1;
        let text = std::fs::read_to_string(path).ok()?;
        let _ = lease.client().track_open(&uri, version).await;
        let _ = lease
            .client()
            .notify(
                "textDocument/didChange",
                serde_json::json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": text }]
                }),
            )
            .await;
        None
    }
}
