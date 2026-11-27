use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError, NodePermission};
use crate::config_client::WriteRoute;
use crate::node_scope::ContextCalls as _;

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

    /// Whether the access's own actor is the node's NAC owner or holds the
    /// `admin` relation, as native NAC reports it to that actor. An
    /// observation only: every operation it gates is still checked natively.
    pub async fn actor_is_native_admin(&self) -> Result<bool, ConfigClientError> {
        let actor = identity::Did::new(
            self.actor_did()
                .ok_or(ConfigClientError::NodePermissionActorRequired)?,
        )
        .map_err(ConfigClientError::NodePermissionIdentity)?;
        Ok(self
            .explain_node_subject(&actor)
            .await?
            .iter()
            .any(|grant| {
                matches!(
                    grant.via,
                    acp::nac::PermissionVia::Owner | acp::nac::PermissionVia::Admin
                )
            }))
    }

    /// Whether native node ACP is `Enabled`, as DefraDB reports it to the
    /// access's actor. Over HTTP the status route requires `NacStatus` while
    /// NAC is enabled (an admin holds it); its body names the status by the
    /// native `NacStatus` display text, and only `enabled` is enabled.
    pub async fn nac_enabled(&self) -> Result<bool, ConfigClientError> {
        #[derive(serde::Deserialize)]
        struct Status {
            #[serde(rename = "Status")]
            status: String,
        }
        match self {
            Self::Local(local) => {
                local
                    .run_owned(move |node| async move {
                        Ok::<_, ConfigClientError>(node.nac_manager().is_enabled().await)
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!("{}/acp/node/status", graphql_api_base(endpoint.url())?);
                let client = graphql::http_client().await?;
                let response = endpoint
                    .send_resolving(|endpoint| endpoint.authorize(client.get(&url)))
                    .await??;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::HttpOperationStatus {
                        operation: "read node ACP status",
                        status,
                    });
                }
                Ok(response.json::<Status>().await?.status == "enabled")
            }
        }
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
                local.require_actor_signer()?;
                local
                    .identity()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let context = local.request_context();
                let subject = subject.clone();
                local
                    .run_owned(move |node| async move {
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
                    .send_resolving(|endpoint| {
                        endpoint.authorize(client.get(&url).query(&[("subject", subject.as_str())]))
                    })
                    .await??;
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

    pub async fn observe_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
    ) -> Result<bool, ConfigClientError> {
        crate::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) => {
                local.require_actor_signer()?;
                let context = local.request_context();
                let (collection, doc_id, relation, target) = (
                    collection.to_owned(),
                    doc_id.to_owned(),
                    relation.to_owned(),
                    target.to_string(),
                );
                local
                    .run_owned(move |node| async move {
                        Ok::<_, ConfigClientError>(
                            node.observe_dac_actor_relationship_as(
                                &context,
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await?,
                        )
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!(
                    "{}/acp/document/relationship",
                    graphql_api_base(endpoint.url())?
                );
                let client = graphql::http_client().await?;
                let response = endpoint
                    .send_resolving(|endpoint| {
                        endpoint.authorize(client.get(&url).query(&[
                            ("collection", collection),
                            ("docID", doc_id),
                            ("relation", relation),
                            ("targetActor", target.as_str()),
                        ]))
                    })
                    .await??;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::HttpOperationStatus {
                        operation: "observe document actor relationship",
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
        crate::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) => {
                local.require_actor_signer()?;
                local
                    .identity()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?;
                let context = local.request_context();
                let (collection, doc_id) = (collection.to_owned(), doc_id.to_owned());
                local
                    .run_owned(move |node| async move {
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
                    .send_resolving(|endpoint| {
                        endpoint.authorize(
                            client
                                .get(&url)
                                .query(&[("collection", collection), ("docID", doc_id)]),
                        )
                    })
                    .await??;
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

    /// The relationships on a document, read without an Agent signature, for
    /// the offline operator CLI: the one access built with the endpoint holding
    /// the client signing key (`with_client_signed_writes_through`), which no
    /// Agent holds. Its actor is the client identity that endpoint proves, and
    /// Native DefraDB still decides the read on it.
    ///
    /// The unsigned path is taken only on that proof, never on the mere absence
    /// of an Agent signer: the runtime and session side build signer-less
    /// [`Self::Local`] accesses too, and any of them asserting an identity must
    /// not inherit this read. Every other access falls to
    /// [`Self::list_document_relationships`], whose `ManagedAgentSignerRequired`
    /// the `workspace-root` listing degrades to `unknown (<reason>)`.
    pub async fn list_document_relationships_as_node(
        &self,
        collection: &str,
        doc_id: &str,
    ) -> Result<Vec<acp::DocumentRelationship>, ConfigClientError> {
        match self {
            Self::Local(local) if local.agent_signer().is_some() => {
                self.list_document_relationships(collection, doc_id).await
            }
            // The client signing key proves the operator; nothing else does.
            Self::Local(local) if local.client_endpoint().is_some() => {
                self.list_document_relationships_unproved(collection, doc_id)
                    .await
            }
            _ => self.list_document_relationships(collection, doc_id).await,
        }
    }

    /// [`Self::list_document_relationships`] without the signer gate, reachable
    /// only through [`Self::list_document_relationships_as_node`]'s proof.
    async fn list_document_relationships_unproved(
        &self,
        collection: &str,
        doc_id: &str,
    ) -> Result<Vec<acp::DocumentRelationship>, ConfigClientError> {
        crate::graphql::validate_collection_identifier(collection)?;
        let Self::Local(local) = self else {
            return self.list_document_relationships(collection, doc_id).await;
        };
        let actor = local
            .identity()
            .cloned()
            .ok_or(ConfigClientError::NodePermissionActorRequired)?;
        let (collection, doc_id) = (collection.to_owned(), doc_id.to_owned());
        local
            .run_owned(move |node| async move {
                let context =
                    defra_core::RequestContext::default().with_identity(Some(actor.to_string()));
                Ok::<_, ConfigClientError>(
                    node.list_dac_relationships_as(&context, &collection, &doc_id)
                        .await?,
                )
            })
            .await
    }

    /// A grant to a member whose Node administration removal is owed is
    /// refused (Lean `Identity.EnrollmentGrant.permissionGrantAdmitted`).
    /// Native NAC decides the change as this access's actor: locally its
    /// identity under its managed signer, over HTTP the endpoint principal.
    /// The runtime principal's own change is issued as the NAC owner, which
    /// is never the Node (`Identity.NodeProvisioning.nacOwner`): native admits
    /// a NAC change with no node exception, so the runtime names the owner in
    /// process and keeps the NAC authority it had as owner without its read
    /// bypass.
    async fn change_node_relation(
        &self,
        relation: &'static str,
        target: &identity::Did,
        delete: bool,
    ) -> Result<(), ConfigClientError> {
        match self.write_route() {
            WriteRoute::Local(local) => {
                local.require_mutation_signer()?;
                let actor = local
                    .identity()
                    .ok_or(ConfigClientError::NodePermissionActorRequired)?
                    .clone();
                let context = local.request_context();
                let target = target.clone();
                local
                    .run_owned(move |node| async move {
                        let _guard = if delete {
                            None
                        } else {
                            Some(admit_permission_grant(&node, &target).await?)
                        };
                        let context = match node.node_identity_did() == Some(actor.as_str()) {
                            true => defra_core::RequestContext::default().with_identity(Some(
                                node.nac_manager()
                                    .owner()
                                    .await
                                    .ok_or(ConfigClientError::NodeAcpOwnerUnknown)?
                                    .to_string(),
                            )),
                            false => context,
                        };
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
            WriteRoute::Http(endpoint) => {
                let path = if delete {
                    "/acp/node/relationship"
                } else {
                    super::owner_staged_route::PERMISSION_GRANT_PATH
                };
                let url = format!("{}{path}", graphql_api_base(endpoint.url())?);
                let client = graphql::http_client().await?;
                let body = serde_json::json!({
                    "Relation": relation, "TargetActor": target.as_str(),
                });
                let response = endpoint
                    .send_resolving(|endpoint| {
                        let request = if delete {
                            client.delete(&url)
                        } else {
                            client.post(&url)
                        };
                        endpoint.authorize(request.json(&body))
                    })
                    .await??;
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

pub(super) async fn admit_permission_grant(
    node: &std::sync::Arc<defra_node::Node>,
    target: &identity::Did,
) -> Result<tokio::sync::OwnedMutexGuard<()>, ConfigClientError> {
    let guard = crate::identity::enrollment_decision_gate(node)
        .lock_owned()
        .await;
    let owed = crate::identity::owed_removal_receipts(node, target.as_str()).await?;
    if !crate::identity::permission_grant_admitted(!owed.is_empty()) {
        return Err(ConfigClientError::AdministrationRemovalPending {
            member: target.to_string(),
        });
    }
    Ok(guard)
}

/// Whether `did` still holds node read permission on `node`, which a
/// subscription's handshake checked once (`tla/SubscriptionStreamEnd.tla`).
/// A node with NAC disabled grants it to every caller, as the handshake does.
/// A DID that does not parse holds nothing. A check that fails is an error,
/// which the caller reads as unreadable (Lean `SubscriptionStream.gate`).
pub async fn holds_node_read(
    node: &defra_node::Node,
    did: &str,
) -> std::result::Result<bool, ConfigClientError> {
    let nac = node.nac_manager();
    if !nac.is_enabled().await {
        return Ok(true);
    }
    let Ok(did) = identity::Did::new(did) else {
        return Ok(false);
    };
    nac.check_permission(&did, acp::nac::NodePermission::DocumentRead)
        .await
        .map_err(ConfigClientError::NativePermissionObservation)
}
