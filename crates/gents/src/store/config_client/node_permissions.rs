use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError, NodePermission};

impl ConfigAccess {
    /// Revoke an individual native node operation; document relationships are independent.
    pub async fn revoke_node_permission(
        &self,
        target: &identity::Did,
        permission: NodePermission,
    ) -> Result<(), ConfigClientError> {
        match self {
            Self::Local(local) => {
                let actor = local
                    .identity()
                    .cloned()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let target = target.clone();
                local
                    .run_owned(move |node| async move {
                        let context = defra_core::RequestContext::default()
                            .with_identity(Some(actor.to_string()));
                        node.delete_nac_actor_relationship_as(
                            &context,
                            permission.as_str(),
                            target.as_str(),
                        )
                        .await?;
                        Ok::<_, ConfigClientError>(())
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!(
                    "{}/acp/node/relationship",
                    graphql_api_base(endpoint.url())?
                );
                let client = graphql::http_client().await?;
                let response = endpoint
                    .authorize(client.delete(url).json(&serde_json::json!({
                        "Relation": permission.as_str(), "TargetActor": target.as_str(),
                    })))?
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::NodePermissionStatus {
                        status,
                        body: response.text().await?,
                    });
                }
                Ok(())
            }
        }
    }
    /// Grant full native node administration, including DAC bypass. This is
    /// an explicit node-administrator operation, not a peer-enrollment grant.
    pub async fn grant_node_admin(&self, target: &identity::Did) -> Result<(), ConfigClientError> {
        match self {
            Self::Local(local) => {
                let actor = local
                    .identity()
                    .cloned()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let target = target.clone();
                local
                    .run_owned(move |node| async move {
                        let context = defra_core::RequestContext::default()
                            .with_identity(Some(actor.to_string()));
                        node.add_nac_actor_relationship_as(&context, "admin", target.as_str())
                            .await?;
                        Ok::<_, ConfigClientError>(())
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!(
                    "{}/acp/node/relationship",
                    graphql_api_base(endpoint.url())?
                );
                let client = graphql::http_client().await?;
                let response = endpoint
                    .authorize(client.post(url).json(&serde_json::json!({
                        "Relation": "admin", "TargetActor": target.as_str(),
                    })))?
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::NodePermissionStatus {
                        status,
                        body: response.text().await?,
                    });
                }
                Ok(())
            }
        }
    }

    /// Grants a node-level operation through DefraDB's node policy. This is
    /// independent of document ACP and does not confer document visibility.
    pub async fn grant_node_permission(
        &self,
        target: &identity::Did,
        permission: NodePermission,
    ) -> Result<(), ConfigClientError> {
        match self {
            Self::Local(local) => {
                let actor = local
                    .identity()
                    .cloned()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let target = target.clone();
                local
                    .run_owned(move |node| async move {
                        let context = defra_core::RequestContext::default()
                            .with_identity(Some(actor.to_string()));
                        node.add_nac_actor_relationship_as(
                            &context,
                            permission.as_str(),
                            target.as_str(),
                        )
                        .await?;
                        Ok::<_, ConfigClientError>(())
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!(
                    "{}/acp/node/relationship",
                    graphql_api_base(endpoint.url())?
                );
                let client = graphql::http_client().await?;
                let response = endpoint
                    .authorize(client.post(url).json(&serde_json::json!({
                        "Relation": permission.as_str(), "TargetActor": target.as_str(),
                    })))?
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::NodePermissionStatus {
                        status,
                        body: response.text().await?,
                    });
                }
                Ok(())
            }
        }
    }
}
