use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use defra_node::QueryRequest;
use serde_json::{json, Value};

use super::apply_txn::TxnBackend;
#[cfg(test)]
use super::fault_injection::{after_successful_mutation_for_test, SUCCESSFUL_MUTATION_FAULT};
use super::native::execute_owned_native;
use super::{graphql, graphql_api_base, retry, ConfigApplyTxn, ConfigClientError, Result};
use crate::store::config_client::write_telemetry::{record_slow_native_phase, ConflictSource};

impl ConfigApplyTxn<'_> {
    pub(crate) async fn delete_verified_configuration_document(
        &self,
        collection: &str,
        doc_id: &str,
        graph: std::sync::Arc<db::block::builder::VerifiedClientMutation>,
    ) -> Result<()> {
        super::super::client_signed_agent::trusted_delete_schema(collection)
            .ok_or(ConfigClientError::ClientConfigurationDeleteInvalid)?;
        if graph.doc_id() != doc_id {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        }
        let mutation = format!(
            r#"mutation {{ delete_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(doc_id),
        );
        let response = self
            .execute_verified_client_mutation(&mutation, &json!({}), graph)
            .await?;
        super::delete_confirmation::confirm_configuration_delete(&response, collection, doc_id)
    }

    /// Delete an explicitly supported bundled document and require native confirmation.
    pub(crate) async fn delete_configuration_document(
        &self,
        collection: &str,
        doc_id: &str,
    ) -> Result<()> {
        let schema = super::super::client_signed_agent::trusted_delete_schema(collection)
            .ok_or(ConfigClientError::ClientConfigurationDeleteInvalid)?;
        self.delete_document_with_schema(collection, doc_id, schema)
            .await
    }

    /// External native deletion can invalidate physical session targets while
    /// product session deletion remains deferred.
    #[cfg(test)]
    pub(crate) async fn delete_session_for_native_fixture(&self, doc_id: &str) -> Result<()> {
        let TxnBackend::Http { endpoint, .. } = &self.backend else {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        };
        if !endpoint.client_signed_agent_creates || endpoint.pack_replay.is_some() {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        }
        endpoint.held_client_signing_capability()?;
        self.delete_document_with_schema(
            gents_protocol::schemas::AGENT_SESSION_NAME,
            doc_id,
            gents_protocol::schemas::AGENT_SESSION,
        )
        .await
    }

    /// A session administrator's signed deletion of an Agent-written session
    /// child, for fixtures that remove receipts beneath a delivered session.
    #[cfg(test)]
    pub(crate) async fn delete_session_child_for_native_fixture(
        &self,
        collection: &str,
        doc_id: &str,
    ) -> Result<()> {
        use gents_protocol::schemas;
        let schema = match collection {
            schemas::TRIGGER_FIRE_NAME => schemas::TRIGGER_FIRE,
            schemas::GOAL_NAME => schemas::GOAL,
            schemas::GOAL_CREATION_CLAIM_NAME => schemas::GOAL_CREATION_CLAIM,
            _ => return Err(ConfigClientError::ClientConfigurationDeleteInvalid),
        };
        let TxnBackend::Http { endpoint, .. } = &self.backend else {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        };
        if !endpoint.client_signed_agent_creates || endpoint.pack_replay.is_some() {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        }
        endpoint.held_client_signing_capability()?;
        self.delete_document_with_schema(collection, doc_id, schema)
            .await
    }

    /// A requester's own signed deletion of its AgentRequest, for fixtures that
    /// observe a request removed beneath its working Agent.
    #[cfg(test)]
    pub(crate) async fn delete_request_for_native_fixture(&self, doc_id: &str) -> Result<()> {
        let TxnBackend::Http { endpoint, .. } = &self.backend else {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        };
        if !endpoint.client_signed_agent_creates || endpoint.pack_replay.is_some() {
            return Err(ConfigClientError::ClientConfigurationDeleteInvalid);
        }
        endpoint.held_client_signing_capability()?;
        self.delete_document_with_schema(
            gents_protocol::schemas::AGENT_REQUEST_NAME,
            doc_id,
            gents_protocol::schemas::AGENT_REQUEST,
        )
        .await
    }

    /// Delete one document, signing it on a client-signed transaction against
    /// the caller's trusted `schema` for `collection`.
    pub(crate) async fn delete_document_with_schema(
        &self,
        collection: &str,
        doc_id: &str,
        schema: &str,
    ) -> Result<()> {
        let mutation = format!(
            r#"mutation {{ delete_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(doc_id),
        );
        if self.has_pack_interpreter() {
            let response = self
                .apply_pack_interpreter_effect(crate::store::config_client::PackMutation::Delete {
                    collection: collection.to_owned(),
                    doc_id: doc_id.to_owned(),
                })
                .await?;
            return super::delete_confirmation::confirm_configuration_delete(
                &response, collection, doc_id,
            );
        }
        let response = match &self.backend {
            TxnBackend::Http { endpoint, id, .. } if endpoint.client_signed_agent_creates => {
                super::super::client_signed_agent::client_signer(endpoint)?;
                let history = defra_http::client_signed_update::history_request(doc_id);
                let variables = history.variables.unwrap_or_else(|| json!({}));
                let observed = self
                    .execute_with_variables(&history.query, &variables)
                    .await?;
                let observed: query::QueryResponse = serde_json::from_value(observed)?;
                let snapshot =
                    defra_http::client_signed_update::snapshot_from_response(doc_id, &observed)
                        .map_err(ConfigClientError::ClientConfigurationGraph)?;
                self.client_signed_write_dispatched
                    .store(true, Ordering::Release);
                let result = super::super::client_signed_agent::execute_delete(
                    endpoint,
                    id,
                    &mutation,
                    schema.to_owned(),
                    collection.to_owned(),
                    doc_id.to_owned(),
                    snapshot,
                )
                .await?;
                self.affected_documents
                    .fetch_add(graphql::affected_documents(&result), Ordering::Relaxed);
                #[cfg(test)]
                after_successful_mutation_for_test(self.operation).await?;
                result
            }
            _ => self.execute(&mutation).await?,
        };
        super::delete_confirmation::confirm_configuration_delete(&response, collection, doc_id)
    }

    pub(crate) async fn execute_desired_state_mutation(
        &self,
        document: &str,
        variables: &Value,
        collection: crate::Collection,
        create: bool,
    ) -> Result<Value> {
        use super::super::client_signed_agent::{
            create_route, trusted_create_schema, update_route, CreateRoute,
        };
        let (intent, http) = match &self.backend {
            TxnBackend::Http { endpoint, .. } => (endpoint.client_signed_agent_creates, true),
            TxnBackend::Embedded { .. } => {
                (self.pack_approval.is_some(), self.pack_approval.is_some())
            }
        };
        if intent && trusted_create_schema(collection).is_none() {
            return Err(ConfigClientError::ClientConfigurationUpdateUnsupported);
        }
        let route = if create {
            create_route(
                intent,
                trusted_create_schema(collection).is_some(),
                true,
                http,
            )
        } else {
            update_route(intent, collection, http)
        };
        if !create && route == CreateRoute::Refuse {
            return Err(ConfigClientError::ClientConfigurationUpdateUnsupported);
        }
        match route {
            CreateRoute::Native => self.execute_with_variables(document, variables).await,
            CreateRoute::Refuse => Err(ConfigClientError::ClientAgentTransportRequired),
            CreateRoute::Signed => {
                let schema = trusted_create_schema(collection)
                    .ok_or(ConfigClientError::ClientConfigurationUpdateUnsupported)?;
                if create {
                    self.execute_client_signed_create(document, variables, schema)
                        .await
                } else {
                    self.execute_client_signed_update(document, variables, schema)
                        .await
                }
            }
        }
    }

    /// Create using the authenticated client's key and an application-owned bundled SDL.
    pub(crate) async fn execute_client_signed_create(
        &self,
        document: &str,
        variables: &Value,
        trusted_schema: &str,
    ) -> Result<Value> {
        if self.has_pack_interpreter() {
            let schemas = query::parse_sdl(trusted_schema)?;
            if schemas.len() != 1 {
                return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
            }
            let input = variables
                .get("input")
                .cloned()
                .ok_or(ConfigClientError::VariablesNotObject)?;
            return self
                .apply_pack_interpreter_effect(crate::store::config_client::PackMutation::Create {
                    collection: schemas[0].name.clone(),
                    input,
                })
                .await;
        }
        let TxnBackend::Http { endpoint, id, .. } = &self.backend else {
            return self.execute_with_variables(document, variables).await;
        };
        super::super::client_signed_agent::client_signer(endpoint)?;
        let schemas = query::parse_sdl(trusted_schema)?;
        if schemas.len() != 1 {
            return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
        }
        self.client_signed_write_dispatched
            .store(true, Ordering::Release);
        let response = super::super::client_signed_agent::execute(
            endpoint,
            id,
            document,
            variables,
            trusted_schema.to_owned(),
            schemas[0].name.clone(),
        )
        .await?;
        self.affected_documents
            .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
        #[cfg(test)]
        after_successful_mutation_for_test(self.operation).await?;
        Ok(response)
    }

    /// Execute a client-approved update using an application-owned bundled SDL.
    /// The existing transaction evaluates the complete mutation predicate.
    pub(crate) async fn execute_client_signed_update(
        &self,
        document: &str,
        variables: &Value,
        trusted_schema: &str,
    ) -> Result<Value> {
        if self.has_pack_interpreter() {
            let schemas = query::parse_sdl(trusted_schema)?;
            if schemas.len() != 1 {
                return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
            }
            let collection = schemas[0].name.clone();
            let doc_id =
                super::super::client_signed_agent::update_target(document, variables, &collection)?;
            let input = variables
                .get("input")
                .cloned()
                .ok_or(ConfigClientError::VariablesNotObject)?;
            return self
                .apply_pack_interpreter_effect(crate::store::config_client::PackMutation::Update {
                    collection,
                    doc_id,
                    input,
                })
                .await;
        }
        let TxnBackend::Http { endpoint, id, .. } = &self.backend else {
            return self.execute_with_variables(document, variables).await;
        };
        super::super::client_signed_agent::client_signer(endpoint)?;
        let schemas = query::parse_sdl(trusted_schema)?;
        if schemas.len() != 1 {
            return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
        }
        let collection = schemas[0].name.clone();
        let doc_id =
            super::super::client_signed_agent::update_target(document, variables, &collection)?;
        let history = defra_http::client_signed_update::history_request(&doc_id);
        let history_variables = history.variables.unwrap_or_else(|| json!({}));
        let observed = self
            .execute_with_variables(&history.query, &history_variables)
            .await?;
        let observed: query::QueryResponse = serde_json::from_value(observed)?;
        let snapshot = defra_http::client_signed_update::snapshot_from_response(&doc_id, &observed)
            .map_err(ConfigClientError::ClientConfigurationGraph)?;
        self.client_signed_write_dispatched
            .store(true, Ordering::Release);
        let response = super::super::client_signed_agent::execute_update(
            endpoint,
            id,
            document,
            variables,
            super::super::client_signed_agent::SignedUpdateTarget {
                schema: trusted_schema.to_owned(),
                collection,
                doc_id,
                snapshot,
            },
        )
        .await?;
        self.affected_documents
            .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
        #[cfg(test)]
        after_successful_mutation_for_test(self.operation).await?;
        Ok(response)
    }

    /// Stage a resource relationship in this transaction.
    pub async fn add_resource_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target_resource: &str,
        target_doc_id: &str,
    ) -> Result<()> {
        let target = super::super::relationships::resource_target(target_resource, target_doc_id)?;
        self.change_document_relationship(collection, doc_id, relation, &target, false)
            .await
    }

    /// Remove a resource relationship in this transaction.
    pub async fn delete_resource_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target_resource: &str,
        target_doc_id: &str,
    ) -> Result<()> {
        let target = super::super::relationships::resource_target(target_resource, target_doc_id)?;
        self.change_document_relationship(collection, doc_id, relation, &target, true)
            .await
    }

    /// Stage a relationship to a named native entity set in this transaction.
    pub async fn add_entity_set_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target_resource: &str,
        target_doc_id: &str,
        target_relation: &str,
    ) -> Result<()> {
        let target = super::super::relationships::entity_set_target(
            target_resource,
            target_doc_id,
            target_relation,
        )?;
        self.change_document_relationship(collection, doc_id, relation, &target, false)
            .await
    }

    /// Remove a relationship to a named native entity set in this transaction.
    pub async fn delete_entity_set_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target_resource: &str,
        target_doc_id: &str,
        target_relation: &str,
    ) -> Result<()> {
        let target = super::super::relationships::entity_set_target(
            target_resource,
            target_doc_id,
            target_relation,
        )?;
        self.change_document_relationship(collection, doc_id, relation, &target, true)
            .await
    }

    /// The staged records are supplied by the trusted host while its plugin
    /// store lease is held; the request actor remains the transaction owner.
    pub(crate) async fn stage_plugin_delete_roots(
        &self,
        previous: &[crate::store::local::ProjectedPlugin],
        installed: &[crate::store::local::ProjectedPlugin],
    ) -> Result<()> {
        use crate::store::local::plugin_delete_source as source;
        if !previous
            .iter()
            .chain(installed)
            .any(|record| record.deletes)
        {
            return Ok(());
        }
        let TxnBackend::Embedded {
            node,
            handle,
            identity,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let actor = identity
            .as_ref()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        let node_did = node
            .node_identity_did()
            .ok_or(ConfigClientError::LocalAccessRequired)?;
        // The node owns the transaction; native NAC judges each changing
        // edge against the explicit actor and admits an unchanged one without
        // authority (Lean `Configuration.sourceChangeAllowed`).
        let context = defra_core::RequestContext::default().with_identity(Some(actor.to_string()));
        let roots = |records: &[crate::store::local::ProjectedPlugin]| {
            let mut roots = std::collections::BTreeSet::new();
            for record in records {
                if !record.deletes {
                    continue;
                }
                for pin in [None, Some(record.digest.as_str())] {
                    roots.insert(source::root(node_did, &record.coordinate, pin));
                }
            }
            roots
        };
        let before = roots(previous);
        let after = roots(installed);
        let mut edges = Vec::new();
        for root in before.difference(&after) {
            edges.push(query::txn::SourcePermission {
                permission: acp::NodePermission::DocumentDelete.as_str().to_owned(),
                parent_source: None,
                subject_kind: "source".into(),
                subject: root.clone(),
                delete: true,
            });
        }
        for root in after {
            edges.push(query::txn::SourcePermission {
                permission: acp::NodePermission::DocumentDelete.as_str().to_owned(),
                parent_source: Some(source::index(node_did)),
                subject_kind: "source".into(),
                subject: root.clone(),
                delete: false,
            });
            edges.push(query::txn::SourcePermission {
                permission: acp::NodePermission::DocumentDelete.as_str().to_owned(),
                parent_source: None,
                subject_kind: "source".into(),
                subject: root,
                delete: false,
            });
        }
        let node = std::sync::Arc::clone(node);
        let handle = handle.clone();
        execute_owned_native(
            async move {
                for edge in edges {
                    node.change_nac_source_permission_for_in_txn(&handle, &context, &edge)
                        .await?;
                }
                Ok::<_, defra_node::Error>(())
            },
            std::sync::Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(|error| ConfigClientError::Node(Box::new(error)))
    }

    /// Stage an actor relationship in this transaction.
    pub async fn add_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
    ) -> Result<()> {
        self.change_document_relationship(collection, doc_id, relation, target.as_str(), false)
            .await
    }

    /// Stage an actor relationship removal in this transaction.
    pub async fn delete_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
    ) -> Result<()> {
        self.change_document_relationship(collection, doc_id, relation, target.as_str(), true)
            .await
    }

    async fn change_document_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
        delete: bool,
    ) -> Result<()> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        if self.has_pack_interpreter() {
            let relationship = super::pack_interpreter::relationship(
                collection, doc_id, relation, target, delete,
            )?;
            return self.apply_pack_interpreter_relationship(relationship).await;
        }
        self.change_document_relationship_unchecked(collection, doc_id, relation, target, delete)
            .await
    }

    pub(super) async fn change_document_relationship_unchecked(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
        delete: bool,
    ) -> Result<()> {
        match &self.backend {
            TxnBackend::Embedded {
                node,
                handle,
                identity,
                native_operations,
                ..
            } => {
                let actor = identity
                    .as_ref()
                    .ok_or(ConfigClientError::RelationshipActorRequired)?;
                let context =
                    defra_core::RequestContext::default().with_identity(Some(actor.to_string()));
                let node = Arc::clone(node);
                let handle = handle.clone();
                let (collection, doc_id, relation, target) = (
                    collection.to_owned(),
                    doc_id.to_owned(),
                    relation.to_owned(),
                    target.to_owned(),
                );
                execute_owned_native(
                    async move {
                        if delete {
                            node.delete_dac_actor_relationship_for_in_txn(
                                &handle,
                                &context,
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await
                        } else {
                            node.add_dac_actor_relationship_for_in_txn(
                                &handle,
                                &context,
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await
                        }
                    },
                    Arc::clone(native_operations),
                    self.operation,
                )
                .await?
                .map_err(native_relationship_error)?;
            }
            TxnBackend::Http {
                endpoint,
                id,
                client,
            } => {
                if endpoint.is_anonymous() || endpoint.principal_did.is_none() {
                    return Err(ConfigClientError::RelationshipActorRequired);
                }
                let url = format!(
                    "{}/acp/document/relationship",
                    graphql_api_base(endpoint.url())?
                );
                let method = if delete {
                    reqwest::Method::DELETE
                } else {
                    reqwest::Method::POST
                };
                let response = endpoint.authorize(client.request(method, url).header("x-defradb-tx", id).json(&json!({
                    "collection": collection, "doc_id": doc_id, "relation": relation, "target_actor": target,
                })))?.send().await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::RelationshipStatus {
                        status,
                        body: response.text().await?,
                    });
                }
            }
        }
        Ok(())
    }

    /// Privileged access observation in this transaction's snapshot.
    /// The caller authenticates `subject` and restricts access to this method;
    /// observing access does not authenticate the subject or change the actor.
    pub async fn observe_document_access(
        &self,
        subject: &identity::Did,
        collection: &str,
        doc_ids: &[String],
        mode: defra_node::DocumentAccessMode,
    ) -> Result<Vec<defra_node::DocumentAccessDecision>> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        let TxnBackend::Embedded {
            node,
            handle,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::TransactionAuthorizationUnsupported);
        };
        let node = Arc::clone(node);
        let handle = handle.clone();
        let subject = subject.clone();
        let collection = collection.to_owned();
        let doc_ids = doc_ids.to_vec();
        execute_owned_native(
            async move {
                node.observe_document_access_in_txn(&handle, &collection, &doc_ids, subject, mode)
                    .await
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(native_observation_error)
    }

    /// The native observations for a destructive admin decision
    /// (`Identity.Configuration.destructiveAdminDecision`): whether native NAC
    /// is enabled and whether it reports this transaction's actor as admin.
    /// Every available read is ACP-filtered. It confers no authority, and
    /// every write it gates is still checked natively. Over HTTP, NAC status
    /// and the admin relation are read as the caller; a caller native refuses
    /// either observation is refused.
    pub(crate) async fn destructive_authority(
        &self,
    ) -> Result<super::super::DestructiveAuthority> {
        let (node, native_operations) = match &self.backend {
            TxnBackend::Embedded {
                node,
                native_operations,
                ..
            } => (node, native_operations),
            TxnBackend::Http { endpoint, .. } => {
                let access = super::super::ConfigAccess::Graphql((*endpoint).clone());
                return Ok(super::super::DestructiveAuthority {
                    nac_enabled: access.nac_enabled().await?,
                    admin: access.actor_is_native_admin().await?,
                    inventory: super::super::InventoryRead::AcpFiltered,
                });
            }
        };
        let actor = identity::Did::new(
            self.actor_did()
                .ok_or(ConfigClientError::NodePermissionActorRequired)?,
        )
        .map_err(ConfigClientError::NodePermissionIdentity)?;
        let node = Arc::clone(node);
        let (nac_enabled, admin) = execute_owned_native(
            async move {
                let nac = node.nac_manager();
                let enabled = nac.is_enabled().await;
                nac.is_admin(&actor).await.map(|admin| (enabled, admin))
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(ConfigClientError::NativeAdminObservation)?;
        Ok(super::super::DestructiveAuthority {
            nac_enabled,
            admin,
            inventory: super::super::InventoryRead::AcpFiltered,
        })
    }

    /// Whether this transaction's actor holds `permission` natively, as NAC
    /// reports it; with NAC disabled no node permission is enforced. An
    /// observation only: every operation it gates is still checked natively.
    pub(crate) async fn actor_holds_node_permission(
        &self,
        permission: super::super::NodePermission,
    ) -> Result<bool> {
        let TxnBackend::Embedded {
            node,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::TransactionAuthorizationUnsupported);
        };
        let actor = identity::Did::new(
            self.actor_did()
                .ok_or(ConfigClientError::NodePermissionActorRequired)?,
        )
        .map_err(ConfigClientError::NodePermissionIdentity)?;
        let node = Arc::clone(node);
        execute_owned_native(
            async move {
                let nac = node.nac_manager();
                if !nac.is_enabled().await {
                    return Ok(true);
                }
                nac.check_permission(&actor, permission).await
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(ConfigClientError::NativePermissionObservation)
    }

    /// Execute exactly once inside this snapshot; conflict replay is the outer `transact` owner's job.
    pub async fn execute(&self, document: &str) -> Result<Value> {
        self.execute_with_variables(document, &json!({})).await
    }

    /// The invocation actor must be the transaction actor. This checks context
    /// integrity only; native DefraDB policies decide document authorization.
    pub(crate) fn validate_effect_actor(&self, actor: &identity::Did) -> Result<()> {
        let matches = match &self.backend {
            TxnBackend::Embedded { node, identity, .. } => {
                if node.node_identity_did() == Some(actor.as_str()) {
                    return Err(ConfigClientError::RuntimeDocumentActor);
                }
                identity.as_ref() == Some(actor)
            }
            TxnBackend::Http { endpoint, .. } => {
                endpoint.principal_did.as_deref() == Some(actor.as_str())
            }
        };
        if !matches {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        Ok(())
    }

    /// Execute with typed GraphQL variables. JSON scalar payloads retain
    /// arbitrary keys and nested empty arrays; ordinary nillable list fields
    /// remain the caller's responsibility.
    pub async fn execute_with_variables(&self, document: &str, variables: &Value) -> Result<Value> {
        if !variables.is_object() {
            return Err(ConfigClientError::VariablesNotObject);
        }
        if self.has_pack_interpreter() {
            graphql::ensure_query_document(document)?;
            if let TxnBackend::Http { endpoint, .. } = &self.backend {
                if let Some(replay) = &endpoint.pack_replay {
                    return Ok(replay.0.read(document, variables).await?);
                }
            }
        }
        #[cfg(test)]
        if !document.trim_start().starts_with("mutation")
            && SUCCESSFUL_MUTATION_FAULT
                .try_with(|fault| {
                    fault
                        .read_storage_failure
                        .is_some_and(|m| document.contains(m))
                })
                .unwrap_or(false)
        {
            return Err(retry::transaction_storage_failure(
                ConfigClientError::InjectedReadStorageFailure,
            ));
        }
        if let TxnBackend::Http { endpoint, .. } = &self.backend {
            if endpoint.client_signed_agent_creates {
                let variables = variables
                    .as_object()
                    .ok_or(ConfigClientError::VariablesNotObject)?
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                let parsed = query::query_parse::parse_request_with_limits(
                    document,
                    Some(&variables),
                    None,
                    query::QueryLimits::default(),
                )?;
                if !super::super::client_signed_agent::ordinary_mutation_allowed(
                    true,
                    matches!(parsed, query::query_parse::ParsedOperation::Mutation { .. }),
                ) {
                    return Err(ConfigClientError::ClientUnsignedMutationUnsupported);
                }
            }
        }
        let response = match &self.backend {
            TxnBackend::Http {
                endpoint,
                id,
                client,
            } => graphql::txn_execute(endpoint, id, client, document, variables)
                .await
                .map_err(retry::transaction_storage_failure)?,
            TxnBackend::Embedded {
                node,
                handle,
                identity,
                native_operations,
                agent_signer,
            } => {
                let started = Instant::now();
                let node = Arc::clone(node);
                let request = QueryRequest::new(document)
                    .with_variables(variables.clone())
                    .with_identity(identity.clone());
                let handle = handle.clone();
                let agent_signer = agent_signer.as_deref().cloned();
                let result = execute_owned_native(
                    async move {
                        match agent_signer {
                            Some(signer) => {
                                node.execute_request_in_txn_with_signer(
                                    request,
                                    &handle,
                                    signer.native(),
                                )
                                .await
                            }
                            None => node.execute_request_in_txn(request, &handle).await,
                        }
                    },
                    Arc::clone(native_operations),
                    self.operation,
                )
                .await;
                record_slow_native_phase(self.operation, "execute", started.elapsed());
                let response = result?;
                if response.is_transaction_conflict() {
                    return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
                }
                if response.has_errors() {
                    return Err(retry::transaction_storage_failure(
                        ConfigClientError::EmbeddedGraphqlErrors {
                            errors: response.errors,
                        },
                    ));
                }
                crate::store::graphql::into_data_envelope(response.data)
            }
        };
        if document.trim_start().starts_with("mutation") {
            self.affected_documents
                .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
            #[cfg(test)]
            after_successful_mutation_for_test(self.operation).await?;
        }
        Ok(response)
    }

    /// Observes authenticated owners for known live documents in this exact
    /// transaction. Unknown owners cannot certify absence or completeness.
    pub async fn document_owners(
        &self,
        collection: &str,
        doc_ids: &[String],
    ) -> Result<Vec<query::owners::DocumentOwnerObservation>> {
        let actor = match &self.backend {
            TxnBackend::Embedded { identity, .. } => identity
                .clone()
                .ok_or(ConfigClientError::RelationshipActorRequired)?,
            TxnBackend::Http { endpoint, .. } => identity::Did::new(
                endpoint
                    .principal_did
                    .as_deref()
                    .ok_or(ConfigClientError::RelationshipActorRequired)?,
            )
            .map_err(ConfigClientError::RelationshipTargetIdentity)?,
        };
        self.observe_document_owners(collection, doc_ids, Some(actor))
            .await
    }

    /// Checks a separately authenticated reader against known documents in this
    /// transaction's snapshot without replacing its actor or signing identity.
    /// The caller binds `reader` to its verified request, not an authored owner
    /// field. An unknown result establishes neither absence nor completeness;
    /// this read creates no document grant.
    pub async fn document_owners_as(
        &self,
        reader: &identity::Did,
        collection: &str,
        doc_ids: &[String],
    ) -> Result<Vec<query::owners::DocumentOwnerObservation>> {
        self.observe_document_owners(collection, doc_ids, Some(reader.clone()))
            .await
    }

    async fn observe_document_owners(
        &self,
        collection: &str,
        doc_ids: &[String],
        reader: Option<identity::Did>,
    ) -> Result<Vec<query::owners::DocumentOwnerObservation>> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        if matches!(&self.backend, TxnBackend::Http { .. }) {
            let actor = reader.ok_or(ConfigClientError::RelationshipActorRequired)?;
            let rows = self
                .http_owner_observations(collection, doc_ids, &actor)
                .await?;
            return Ok(rows
                .into_iter()
                .map(|row| query::owners::DocumentOwnerObservation {
                    doc_id: row.doc_id,
                    visible: row.visible,
                    owner: row.is_owner.then(|| actor.clone()),
                })
                .collect());
        }
        let TxnBackend::Embedded {
            node,
            handle,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::OwnerObservationRequiresEmbedded);
        };
        let node = Arc::clone(node);
        let handle = handle.clone();
        let collection = collection.to_owned();
        let doc_ids = doc_ids.to_vec();
        execute_owned_native(
            async move {
                node.observe_document_owners_in_txn(&handle, &collection, &doc_ids, reader)
                    .await
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(native_observation_error)
    }

    /// Observe only whether a normally readable document has a registered
    /// native owner, without substituting the caller for that owner. An older
    /// HTTP endpoint proves registration only when the caller is that owner.
    pub(crate) async fn document_owner_registration(
        &self,
        collection: &str,
        doc_ids: &[String],
    ) -> Result<Vec<OwnerRegistrationObservation>> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        match &self.backend {
            TxnBackend::Http { endpoint, .. } => {
                let actor = identity::Did::new(
                    endpoint
                        .principal_did
                        .as_deref()
                        .ok_or(ConfigClientError::RelationshipActorRequired)?,
                )
                .map_err(ConfigClientError::RelationshipTargetIdentity)?;
                let rows = self
                    .http_owner_observations(collection, doc_ids, &actor)
                    .await?;
                rows.into_iter()
                    .map(|row| {
                        let registered = row.registered()?;
                        Ok(OwnerRegistrationObservation {
                            doc_id: row.doc_id,
                            visible: row.visible,
                            registered,
                        })
                    })
                    .collect()
            }
            TxnBackend::Embedded { .. } => Ok(self
                .document_owners(collection, doc_ids)
                .await?
                .into_iter()
                .map(|row| OwnerRegistrationObservation {
                    doc_id: row.doc_id,
                    visible: row.visible,
                    registered: row.visible && row.owner.is_some(),
                })
                .collect()),
        }
    }

    async fn http_owner_observations(
        &self,
        collection: &str,
        doc_ids: &[String],
        actor: &identity::Did,
    ) -> Result<Vec<HttpOwnerObservation>> {
        let TxnBackend::Http {
            endpoint,
            id,
            client,
        } = &self.backend
        else {
            return Err(ConfigClientError::OwnerObservationRequiresEmbedded);
        };
        if endpoint.is_anonymous() || endpoint.principal_did.as_deref() != Some(actor.as_str()) {
            return Err(ConfigClientError::OwnerObservationRequiresEmbedded);
        }
        let url = format!(
            "{}/acp/document/is-owner",
            graphql_api_base(endpoint.url())?
        );
        let response = endpoint
            .authorize(client.post(url).header("x-defradb-tx", id).json(&json!({
                "collection": collection, "doc_ids": doc_ids,
            })))?
            .send()
            .await?
            .error_for_status()?;
        let response: HttpOwnerObservations = response.json().await?;
        if response.documents.len() != doc_ids.len()
            || response
                .documents
                .iter()
                .zip(doc_ids)
                .any(|(row, id)| row.doc_id != *id || row.registered().is_err())
        {
            return Err(ConfigClientError::InvalidOwnerObservation);
        }
        Ok(response.documents)
    }

    /// Transaction-bound delegated visibility is unavailable on this backend.
    pub async fn document_visibility_as(
        &self,
        _actor: identity::Did,
        _collection: &str,
        _doc_ids: &[String],
    ) -> Result<Vec<query::owners::DocumentOwnerObservation>> {
        Err(ConfigClientError::TransactionAuthorizationUnsupported)
    }

    pub async fn collection_version(&self, collection: &str) -> Result<Option<Value>> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        match &self.backend {
            TxnBackend::Embedded { node, .. } => node
                .get_collection(collection)?
                .map(serde_json::to_value)
                .transpose()
                .map_err(ConfigClientError::SerializeCollectionVersion),
            TxnBackend::Http {
                endpoint,
                client,
                id,
                ..
            } => {
                let url = format!(
                    "{}/collections?get_inactive=true",
                    graphql_api_base(endpoint.url())?
                );
                let versions: Value = endpoint
                    .authorize(client.get(&url).header("x-defradb-tx", id))?
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                Ok(versions.as_array().and_then(|versions| {
                    versions
                        .iter()
                        .find(|version| {
                            version.get("Name").and_then(Value::as_str) == Some(collection)
                                && version
                                    .get("IsActive")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true)
                        })
                        .cloned()
                }))
            }
        }
    }

    pub(crate) async fn execute_local_response(
        &self,
        document: &str,
    ) -> Result<defra_node::QueryResponse> {
        let TxnBackend::Embedded {
            identity,
            agent_signer,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        self.execute_local_response_with_actor(
            document,
            identity.clone(),
            agent_signer.as_deref().cloned(),
        )
        .await
    }

    /// Read only account-selection metadata as this transaction's bound
    /// runtime. Native document ACP still filters and authorizes the read.
    /// The credential owner supplies `query`; store holds no OAuthCredential
    /// selection of its own.
    pub(crate) async fn execute_runtime_account_metadata_query(
        &self,
        node_did: &str,
        query: &str,
    ) -> Result<defra_node::QueryResponse> {
        self.execute_runtime_metadata_query(node_did, query).await
    }

    /// Read only the runtime Node's hop cap as this transaction's bound
    /// runtime. Native document ACP still filters the Node row.
    pub(super) async fn execute_runtime_hop_cap_query(
        &self,
        node_did: &str,
    ) -> Result<defra_node::QueryResponse> {
        self.execute_runtime_metadata_query(
            node_did,
            &super::runtime_hop_cap::runtime_hop_cap_query(node_did),
        )
        .await
    }

    async fn execute_runtime_metadata_query(
        &self,
        node_did: &str,
        query: &str,
    ) -> Result<defra_node::QueryResponse> {
        let TxnBackend::Embedded {
            node,
            agent_signer: Some(agent_signer),
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        if node.node_identity_did() != Some(node_did)
            || agent_signer.node_did() != node_did
            || self.actor_did() != Some(agent_signer.did())
        {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let runtime =
            identity::Did::new(node_did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        graphql::ensure_query_document(query)?;
        self.execute_local_response_with_actor(query, Some(runtime), None)
            .await
    }

    async fn execute_local_response_with_actor(
        &self,
        document: &str,
        identity: Option<identity::Did>,
        agent_signer: Option<crate::store::identity::ManagedAgentSigner>,
    ) -> Result<defra_node::QueryResponse> {
        let TxnBackend::Embedded {
            node,
            handle,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let started = Instant::now();
        let node = Arc::clone(node);
        let request = QueryRequest::new(document).with_identity(identity);
        let handle = handle.clone();
        let result = execute_owned_native(
            async move {
                match agent_signer {
                    Some(signer) => {
                        node.execute_request_in_txn_with_signer(request, &handle, signer.native())
                            .await
                    }
                    None => node.execute_request_in_txn(request, &handle).await,
                }
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await;
        record_slow_native_phase(self.operation, "execute", started.elapsed());
        let response = result?;
        if response.is_transaction_conflict() {
            return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
        }
        if response.has_errors() {
            return Err(retry::transaction_storage_failure(
                ConfigClientError::EmbeddedGraphqlErrors {
                    errors: response.errors,
                },
            ));
        }
        if document.trim_start().starts_with("mutation") {
            self.affected_documents.fetch_add(
                crate::store::graphql::mutation_affected_documents(&response) as u64,
                Ordering::Relaxed,
            );
            #[cfg(test)]
            after_successful_mutation_for_test(self.operation).await?;
        }
        Ok(response)
    }
}

fn native_observation_error(error: query::error::QueryError) -> ConfigClientError {
    match error {
        query::error::QueryError::TransactionConflict(_) => {
            retry::transaction_conflict(ConflictSource::TypedError)
        }
        error => retry::transaction_storage_failure(error.into()),
    }
}

pub(super) fn native_relationship_error(error: defra_node::Error) -> ConfigClientError {
    match error {
        defra_node::Error::TransactionConflict
        | defra_node::Error::Query(query::error::QueryError::TransactionConflict(_)) => {
            retry::transaction_conflict(ConflictSource::TypedError)
        }
        error => retry::transaction_storage_failure(error.into()),
    }
}

#[derive(serde::Deserialize)]
struct HttpOwnerObservations {
    documents: Vec<HttpOwnerObservation>,
}

#[derive(Debug)]
pub(crate) struct OwnerRegistrationObservation {
    pub(crate) doc_id: String,
    pub(crate) visible: bool,
    pub(crate) registered: bool,
}

#[derive(serde::Deserialize)]
struct HttpOwnerObservation {
    doc_id: String,
    visible: bool,
    is_owner: bool,
    #[serde(default)]
    owner_registered: Option<bool>,
}

impl HttpOwnerObservation {
    fn registered(&self) -> Result<bool> {
        if (self.is_owner && !self.visible)
            || (self.owner_registered == Some(true) && !self.visible)
            || (self.owner_registered == Some(false) && self.is_owner)
        {
            return Err(ConfigClientError::InvalidOwnerObservation);
        }
        Ok(self.visible && (self.is_owner || self.owner_registered.unwrap_or(false)))
    }
}

#[cfg(test)]
mod owner_registration_model_tests {
    use super::HttpOwnerObservation;

    #[test]
    fn http_registration_observations_match_executed_model() {
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().identity_owner_registration_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let row = HttpOwnerObservation {
                doc_id: "document".into(),
                visible: case["visible"].as_bool().unwrap(),
                is_owner: case["is_owner"].as_bool().unwrap(),
                owner_registered: case["owner_registered"].as_bool(),
            };
            assert_eq!(
                row.registered().ok(),
                case["expected_registered"].as_bool(),
                "{}",
                case["name"]
            );
        }
    }
}
