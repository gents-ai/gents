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

    /// Every way `subject` holds each node permission, as native NAC reports it
    /// to the access's actor. Native allows the subject itself, an admin, or a
    /// caller holding `NacStatus`; any other caller is refused by DefraDB.
    pub async fn explain_node_subject(
        &self,
        subject: &identity::Did,
    ) -> Result<Vec<acp::nac::PermissionProvenance>, ConfigClientError> {
        match self {
            Self::Local(local) => {
                let actor = local
                    .identity()
                    .cloned()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let subject = subject.clone();
                local
                    .run_owned(move |node| async move {
                        let context = defra_core::RequestContext::default()
                            .with_identity(Some(actor.to_string()));
                        Ok::<_, ConfigClientError>(
                            node.explain_nac_subject_as(&context, subject.as_str())
                                .await?,
                        )
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!("{}/acp/node/explain", graphql_api_base(endpoint.url())?);
                let client = graphql::http_client().await?;
                let response = endpoint
                    .authorize(client.get(url).query(&[("subject", subject.as_str())]))?
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::HttpOperationStatus {
                        operation: "explain node subject",
                        status,
                    });
                }
                Ok(response.json().await?)
            }
        }
    }

    /// Every relationship on a document, for an actor that owns it or holds a
    /// relation managing its resource. Native DefraDB decides the gate.
    pub async fn list_document_relationships(
        &self,
        collection: &str,
        doc_id: &str,
    ) -> Result<Vec<acp::DocumentRelationship>, ConfigClientError> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) => {
                let actor = local
                    .identity()
                    .cloned()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let (collection, doc_id) = (collection.to_owned(), doc_id.to_owned());
                local
                    .run_owned(move |node| async move {
                        let context = defra_core::RequestContext::default()
                            .with_identity(Some(actor.to_string()));
                        Ok::<_, ConfigClientError>(
                            node.list_dac_relationships_as(&context, &collection, &doc_id)
                                .await?,
                        )
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!(
                    "{}/acp/document/relationships",
                    graphql_api_base(endpoint.url())?
                );
                let client = graphql::http_client().await?;
                let response = endpoint
                    .authorize(
                        client
                            .get(url)
                            .query(&[("collection", collection), ("docID", doc_id)]),
                    )?
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::HttpOperationStatus {
                        operation: "list document relationships",
                        status,
                    });
                }
                Ok(response.json().await?)
            }
        }
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
