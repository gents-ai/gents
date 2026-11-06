use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError, NodePermission};

const ADMIN_RELATION: &str = "admin";

impl ConfigAccess {
    /// Revoke an individual native node operation; document relationships are independent.
    pub async fn revoke_node_permission(
        &self,
        target: &identity::Did,
        permission: NodePermission,
    ) -> Result<(), ConfigClientError> {
        self.change_node_relation(permission.as_str(), target, true)
            .await
    }

    /// Grant full native node administration, including DAC bypass. This is
    /// an explicit node-administrator operation, not a peer-enrollment grant.
    pub async fn grant_node_admin(&self, target: &identity::Did) -> Result<(), ConfigClientError> {
        self.change_node_relation(ADMIN_RELATION, target, false)
            .await
    }

    /// Remove a direct native node administration grant. The NAC owner stays
    /// an administrator regardless.
    pub async fn revoke_node_admin(&self, target: &identity::Did) -> Result<(), ConfigClientError> {
        self.change_node_relation(ADMIN_RELATION, target, true)
            .await
    }

    /// Grants a node-level operation through DefraDB's node policy. This is
    /// independent of document ACP and does not confer document visibility.
    pub async fn grant_node_permission(
        &self,
        target: &identity::Did,
        permission: NodePermission,
    ) -> Result<(), ConfigClientError> {
        self.change_node_relation(permission.as_str(), target, false)
            .await
    }

    /// Native NAC decides the change as this access's actor: locally its
    /// identity, over HTTP the endpoint principal.
    async fn change_node_relation(
        &self,
        relation: &'static str,
        target: &identity::Did,
        delete: bool,
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
                        if delete {
                            node.delete_nac_actor_relationship_as(
                                &context,
                                relation,
                                target.as_str(),
                            )
                            .await?;
                        } else {
                            node.add_nac_actor_relationship_as(&context, relation, target.as_str())
                                .await?;
                        }
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
                let body = serde_json::json!({
                    "Relation": relation, "TargetActor": target.as_str(),
                });
                let request = if delete {
                    client.delete(url)
                } else {
                    client.post(url)
                };
                let response = endpoint.authorize(request.json(&body))?.send().await?;
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
