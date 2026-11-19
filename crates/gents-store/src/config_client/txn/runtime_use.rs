use serde_json::Value;

use super::apply_txn::TxnBackend;
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};

type Result<T, E = ConfigClientError> = std::result::Result<T, E>;

impl ConfigApplyTxn<'_> {
    /// Whether the hosting runtime `runtime` may use each document, on this
    /// transaction's snapshot (Lean `Identity.Household.runtimeMayUse`): its
    /// document ACP `read` answer, asked as a named capability check, which
    /// carries none of the `bypass-dac` read the node key holds. A document
    /// that is missing or unregistered is not usable. Ask it in the
    /// transaction that makes the use (`queued_revocation_needs_current_permission`).
    pub async fn runtime_may_use(
        &self,
        runtime: &identity::Did,
        collection: &str,
        doc_ids: &[String],
    ) -> Result<Vec<bool>> {
        let mut usable = Vec::with_capacity(doc_ids.len());
        for chunk in doc_ids.chunks(query::owners::MAX_OWNER_OBSERVATIONS) {
            let decisions = self
                .observe_document_access(
                    runtime,
                    collection,
                    chunk,
                    defra_node::DocumentAccessMode::Named {
                        permission: "read".to_owned(),
                        node_permission: acp::nac::NodePermission::DocumentRead,
                    },
                )
                .await?;
            usable.extend(
                decisions.into_iter().map(|decision| {
                    matches!(decision, defra_node::DocumentAccessDecision::Allowed)
                }),
            );
        }
        Ok(usable)
    }

    /// The hosting runtime this transaction reads as, if it reads past
    /// document ACP: the node key, while node access control is enabled. NAC
    /// grants `bypass-dac` only while it is on (`Identity.Household.readObserved`),
    /// so with NAC off the node key's read is already document ACP's answer and
    /// there is nothing to filter. A served runtime never runs with NAC off
    /// (`Identity.NodeProvisioning.runtimeMayStart`).
    async fn hosting_runtime_reader(&self) -> Result<Option<identity::Did>> {
        let TxnBackend::Embedded {
            node,
            identity,
            native_operations,
            ..
        } = &self.backend
        else {
            return Ok(None);
        };
        let Some(runtime) = identity
            .as_ref()
            .filter(|actor| node.node_identity_did() == Some(actor.as_str()))
            .cloned()
        else {
            return Ok(None);
        };
        let node = std::sync::Arc::clone(node);
        let nac_enabled = super::native::execute_owned_native(
            async move { node.nac_manager().is_enabled().await },
            std::sync::Arc::clone(native_operations),
            self.operation,
        )
        .await?;
        Ok(nac_enabled.then_some(runtime))
    }

    /// Drops from `response`'s `collection` rows those `runtime` may not use
    /// (`Identity.Household.usedRead`). Each row must select `_docID`.
    pub async fn retain_runtime_usable(
        &self,
        runtime: &identity::Did,
        response: &mut Value,
        collection: &str,
    ) -> Result<()> {
        let Some(rows) = response
            .get_mut("data")
            .and_then(|data| data.get_mut(collection))
            .and_then(Value::as_array_mut)
        else {
            return Ok(());
        };
        if rows.is_empty() {
            return Ok(());
        }
        let doc_ids = rows
            .iter()
            .map(|row| {
                row["_docID"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(ConfigClientError::RuntimeUseRowWithoutDocId)
            })
            .collect::<Result<Vec<_>>>()?;
        let usable = self.runtime_may_use(runtime, collection, &doc_ids).await?;
        let mut usable = usable.into_iter();
        rows.retain(|_| usable.next().unwrap_or(false));
        Ok(())
    }

    /// Whether this transaction's reader may use each document: the hosting
    /// runtime's [`Self::runtime_may_use`] when it reads as that runtime, and
    /// otherwise every document it read, since any other actor's read is
    /// already document ACP's answer (`without_bypass_document_acp_decides`).
    pub async fn reader_may_use(&self, collection: &str, doc_ids: &[String]) -> Result<Vec<bool>> {
        match self.hosting_runtime_reader().await? {
            Some(runtime) => self.runtime_may_use(&runtime, collection, doc_ids).await,
            None => Ok(vec![true; doc_ids.len()]),
        }
    }

    /// [`Self::execute`] for a read whose rows the reader goes on to use. When
    /// the transaction reads as the hosting runtime, rows it may not use are
    /// dropped; any other actor's read is already document ACP's answer
    /// (`without_bypass_document_acp_decides`).
    pub async fn execute_for_use(&self, document: &str, collection: &str) -> Result<Value> {
        let response = self.execute(document).await?;
        self.retain_reader_usable(response, collection).await
    }

    /// [`Self::run`] for a read whose rows the reader goes on to use, as
    /// [`Self::execute_for_use`].
    pub async fn run_for_use(
        &self,
        rendered: &gents_protocol::document_query::Rendered,
        collection: &str,
    ) -> Result<Value> {
        let response = self.run(rendered).await?;
        self.retain_reader_usable(response, collection).await
    }

    async fn retain_reader_usable(&self, mut response: Value, collection: &str) -> Result<Value> {
        if let Some(runtime) = self.hosting_runtime_reader().await? {
            self.retain_runtime_usable(&runtime, &mut response, collection)
                .await?;
        }
        Ok(response)
    }
}

impl ConfigAccess {
    /// [`ConfigApplyTxn::execute_for_use`] on its own read-only snapshot.
    /// Access that does not read as the hosting runtime executes the read as is.
    pub async fn execute_for_use(&self, document: &str, collection: &str) -> Result<Value> {
        let hosting = match self {
            Self::Local(local) => local
                .identity()
                .is_some_and(|actor| local.node().node_identity_did() == Some(actor.as_str())),
            Self::Graphql(_) => false,
        };
        if !hosting {
            return self.execute(document).await;
        }
        self.transact_readonly("runtime_use.read", |txn| {
            Box::pin(async move { txn.execute_for_use(document, collection).await })
        })
        .await
    }
}
