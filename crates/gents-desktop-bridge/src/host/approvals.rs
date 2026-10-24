use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use gents::plugin::approval;
use gents_desktop_core::client::ClientCore;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::approvals_watch::{watch_queue, PromptSubscriptions, HEARTBEAT_INTERVAL};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::plugin_access::{PluginApprovalView, PluginApprovalsView};

#[derive(Default)]
struct Subscriptions {
    closed: bool,
    nodes: BTreeMap<String, NodeWatch>,
}

struct NodeWatch {
    home: PathBuf,
    subscriptions: PromptSubscriptions,
    task: JoinHandle<()>,
}

impl Drop for NodeWatch {
    fn drop(&mut self) {
        self.subscriptions.remove_window("attachment");
        self.task.abort();
    }
}

/// A native attachment advertises only the answer surfaces it owns. Closing
/// the attachment retires them before submitted mutations are drained.
pub(super) struct AttachmentApprovals {
    subscriptions: Mutex<Subscriptions>,
    changes: watch::Sender<()>,
}

impl Default for AttachmentApprovals {
    fn default() -> Self {
        Self {
            subscriptions: Mutex::new(Subscriptions::default()),
            changes: watch::channel(()).0,
        }
    }
}

impl AttachmentApprovals {
    pub fn changes(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub async fn subscribe(
        &self,
        core: &ClientCore,
        node_did: &str,
        token: &str,
    ) -> Result<(), BridgeError> {
        if token.is_empty() || token.len() > 128 {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "invalid approval subscription",
            ));
        }
        let home = local_home(core, node_did).await?;
        self.subscribe_home(node_did, token, home)
    }

    fn subscribe_home(
        &self,
        node_did: &str,
        token: &str,
        home: PathBuf,
    ) -> Result<(), BridgeError> {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("approval subscription lock poisoned");
        if subscriptions.closed {
            return Err(BridgeError::untyped("the approval attachment closed"));
        }
        if subscriptions.nodes.len() >= 32 && !subscriptions.nodes.contains_key(node_did) {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "too many approval surfaces",
            ));
        }
        if subscriptions
            .nodes
            .get(node_did)
            .is_some_and(|scope| scope.home != home)
        {
            return Err(BridgeError::untyped(
                "the runtime home changed; close and reopen access requests",
            ));
        }
        approval::prepare_listener(&home).map_err(super::backend_error)?;
        let scope = subscriptions
            .nodes
            .entry(node_did.to_owned())
            .or_insert_with(|| {
                let mut subscriptions = PromptSubscriptions::default();
                subscriptions.subscribe("attachment", token.to_owned());
                let receiver = subscriptions.changes();
                let changes = self.changes.clone();
                let task = tokio::spawn(watch_queue(
                    home.clone(),
                    HEARTBEAT_INTERVAL,
                    receiver,
                    move |changed| {
                        if changed {
                            changes.send_replace(());
                        }
                    },
                ));
                NodeWatch {
                    home,
                    subscriptions,
                    task,
                }
            });
        if scope.subscriptions.token_count() >= 32
            && !scope.subscriptions.contains_token("attachment", token)
        {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "too many approval surfaces",
            ));
        }
        scope
            .subscriptions
            .subscribe("attachment", token.to_owned());
        Ok(())
    }

    pub fn unsubscribe(&self, token: &str) {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("approval subscription lock poisoned");
        subscriptions.nodes.retain(|_, scope| {
            scope.subscriptions.unsubscribe("attachment", token);
            scope.subscriptions.contains_window("attachment")
        });
    }

    async fn home(
        &self,
        core: &ClientCore,
        node_did: &str,
        token: &str,
    ) -> Result<PathBuf, BridgeError> {
        let home = local_home(core, node_did).await?;
        let subscriptions = self
            .subscriptions
            .lock()
            .expect("approval subscription lock poisoned");
        if subscriptions.closed
            || !subscriptions.nodes.get(node_did).is_some_and(|scope| {
                scope.home == home && scope.subscriptions.contains_token("attachment", token)
            })
        {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "open access requests before reading or answering a question",
            ));
        }
        Ok(home)
    }

    pub async fn pending(
        &self,
        core: &ClientCore,
        node_did: &str,
        token: &str,
    ) -> Result<PluginApprovalsView, BridgeError> {
        let home = self.home(core, node_did, token).await?;
        let requests = tokio::task::spawn_blocking(move || approval::pending_requests(&home))
            .await
            .map_err(super::backend_error)?
            .map_err(super::backend_error)?;
        Ok(PluginApprovalsView {
            requests: requests.iter().map(PluginApprovalView::from).collect(),
        })
    }

    pub async fn decide(
        &self,
        core: &ClientCore,
        node_did: &str,
        token: &str,
        id: &str,
        decision: &str,
    ) -> Result<(), BridgeError> {
        let answer = crate::plugin_access::answer(decision)?;
        let home = self.home(core, node_did, token).await?;
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || approval::decide(&home, &id, answer))
            .await
            .map_err(super::backend_error)?
            .map_err(super::backend_error)
    }

    pub fn close(&self) {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("approval subscription lock poisoned");
        subscriptions.closed = true;
        subscriptions.nodes.clear();
    }
}

impl Drop for AttachmentApprovals {
    fn drop(&mut self) {
        self.close();
    }
}

pub(super) async fn peer(
    core: &ClientCore,
    node_did: &str,
) -> Result<gents_desktop_core::client::PeerRecord, BridgeError> {
    let records = core.peer_records().await;
    let mut records = records.iter().filter(|record| record.node_did == node_did);
    let record = records.next().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "this runtime has no local approval queue",
        )
    })?;
    if records.next().is_some() {
        return Err(BridgeError::untyped("the runtime record is ambiguous"));
    }
    Ok(record.clone())
}

async fn local_home(core: &ClientCore, node_did: &str) -> Result<PathBuf, BridgeError> {
    let record = peer(core, node_did).await?;
    gents_desktop_core::local_runtime::hosted_runtime_home(&record)
        .ok()
        .and_then(|home| home.canonicalize().ok())
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "access requests require a connected local runtime home",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_an_attachment_retires_every_prompt_and_rejects_late_subscriptions() {
        let home = tempfile::tempdir().unwrap();
        let attachment = AttachmentApprovals::default();
        attachment
            .subscribe_home("node", "first", home.path().to_owned())
            .unwrap();
        attachment
            .subscribe_home("node", "second", home.path().to_owned())
            .unwrap();
        attachment.unsubscribe("first");
        assert_eq!(
            attachment.subscriptions.lock().unwrap().nodes["node"]
                .subscriptions
                .token_count(),
            1
        );
        attachment.close();
        assert!(attachment.subscriptions.lock().unwrap().nodes.is_empty());
        assert!(attachment
            .subscribe_home("node", "late", home.path().to_owned())
            .is_err());
    }

    #[tokio::test]
    async fn an_unwritable_queue_does_not_install_an_answer_surface() {
        let home = tempfile::tempdir().unwrap();
        let queue = approval::queue_dir(home.path());
        std::fs::write(queue.parent().unwrap(), b"not a directory").unwrap();
        let attachment = AttachmentApprovals::default();
        assert!(attachment
            .subscribe_home("node", "surface", home.path().to_owned())
            .is_err());
        assert!(attachment.subscriptions.lock().unwrap().nodes.is_empty());
    }

    #[tokio::test]
    async fn prompt_tokens_and_runtime_homes_cannot_cross_attachments() {
        let home = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first = AttachmentApprovals::default();
        let other = AttachmentApprovals::default();
        first
            .subscribe_home("node", "same", home.path().to_owned())
            .unwrap();
        other.unsubscribe("same");
        assert_eq!(first.subscriptions.lock().unwrap().nodes.len(), 1);
        assert!(first
            .subscribe_home("node", "new", second.path().to_owned())
            .is_err());
        first.unsubscribe("same");
        assert!(first.subscriptions.lock().unwrap().nodes.is_empty());
    }
}
