use anyhow::{Context, Result};
use gents_protocol::{
    client_access::{ClientAccessDecision, ClientAccessReceipt, DecideClientAccess},
    runtime_connection::RuntimeConnection,
};

use super::ClientRuntime;

impl ClientRuntime {
    pub async fn refresh_local_runtime(
        &self,
        home: &std::path::Path,
        label: &str,
    ) -> Result<RuntimeConnection> {
        let discovery = gents::client_runtime::discover_standard_runtime(home).await?;
        let mut connection = RuntimeConnection {
            node_did: discovery.node_did,
            label: label.to_owned(),
            graphql: discovery.graphql,
            local_node_home: Some(home.to_string_lossy().into_owned()),
            access_request: None,
            access_state: None,
        };
        if let Some(retained) = self
            .directory
            .records()
            .into_iter()
            .find(|retained| retained.node_did == connection.node_did)
        {
            connection.access_request = retained.access_request;
            connection.access_state = retained.access_state;
        }
        self.connect_runtime(connection.clone()).await?;
        Ok(connection)
    }

    pub async fn connect_runtime(&self, connection: RuntimeConnection) -> Result<()> {
        anyhow::ensure!(
            connection.validate(),
            "runtime connection has an invalid binding"
        );
        anyhow::ensure!(
            connection
                .access_request
                .as_ref()
                .is_none_or(|request| request.client_did == self.node_identity.did()),
            "runtime access request belongs to another client"
        );
        self.runtime_endpoints
            .endpoint(&connection.node_did, &connection.graphql)?;
        self.directory.upsert(connection).await?;
        Ok(())
    }

    pub async fn request_runtime_access(
        &self,
        connection: RuntimeConnection,
    ) -> Result<ClientAccessReceipt> {
        let _access = self.access_requests.lock().await;
        self.request_runtime_access_locked(connection).await
    }

    async fn request_runtime_access_locked(
        &self,
        mut connection: RuntimeConnection,
    ) -> Result<ClientAccessReceipt> {
        let endpoint = self
            .runtime_endpoints
            .endpoint(&connection.node_did, &connection.graphql)?;
        let request = endpoint
            .prepare_runtime_access_request(
                &connection.node_did,
                uuid::Uuid::new_v4().to_string(),
                connection.label.clone(),
            )
            .await?;
        connection.access_request = Some(request.clone());
        connection.access_state = None;
        self.connect_runtime(connection.clone()).await?;
        let receipt = endpoint.request_runtime_access(&request).await?;
        connection.access_state = Some(receipt.state);
        self.directory.upsert(connection.clone()).await?;
        self.request_runtime_reconnect(Some(&connection.node_did))
            .await;
        Ok(receipt)
    }

    pub async fn runtime_access_status(&self, node_did: &str) -> Result<ClientAccessReceipt> {
        let _access = self.access_requests.lock().await;
        let mut connection = self
            .directory
            .records()
            .into_iter()
            .find(|connection| connection.node_did == node_did)
            .context("runtime connection is not configured")?;
        let request = connection
            .access_request
            .as_ref()
            .context("this connection has no retained access request")?;
        let receipt = match self
            .runtime_endpoints
            .endpoint(node_did, &connection.graphql)?
            .request_runtime_access(request)
            .await
        {
            Ok(receipt) => receipt,
            Err(
                gents_store::config_client::runtime_access::RuntimeAccessError::ExpiredUnretained,
            ) => {
                return self.request_runtime_access_locked(connection).await;
            }
            Err(error) => return Err(error.into()),
        };
        if connection.access_state != Some(receipt.state) {
            connection.access_state = Some(receipt.state);
            self.directory.upsert(connection).await?;
            self.stores.ensure(node_did).request_resync();
        }
        Ok(receipt)
    }

    pub async fn list_runtime_access_requests(
        &self,
        node_did: &str,
    ) -> Result<Vec<ClientAccessReceipt>> {
        Ok(gents_store::identity::client_access::list_requests(
            &self.operator_access(node_did)?,
            node_did,
        )
        .await?)
    }

    pub async fn decide_runtime_access(
        &self,
        node_did: &str,
        request_id: &str,
        decision: ClientAccessDecision,
    ) -> Result<ClientAccessReceipt> {
        let endpoint = self
            .operator_graphql(node_did)
            .context("runtime connection is not configured")?;
        let receipt = endpoint
            .decide_runtime_access(&DecideClientAccess {
                node_did: node_did.to_owned(),
                request_id: request_id.to_owned(),
                decision,
            })
            .await?;
        self.stores.ensure(node_did).request_resync();
        self.request_runtime_reconnect(Some(node_did)).await;
        Ok(receipt)
    }

    pub async fn rename_runtime(&self, node_did: &str, label: &str) -> Result<()> {
        let _access = self.access_requests.lock().await;
        let mut connection = self
            .directory
            .records()
            .into_iter()
            .find(|connection| connection.node_did == node_did)
            .context("runtime connection is not configured")?;
        anyhow::ensure!(!label.trim().is_empty(), "runtime label is required");
        connection.label = label.trim().to_owned();
        self.directory.upsert(connection).await?;
        Ok(())
    }

    pub async fn forget_runtime(&self, node_did: &str) -> Result<bool> {
        let _access = self.access_requests.lock().await;
        Ok(self.directory.remove(node_did).await?)
    }
}

#[cfg(test)]
mod native;
