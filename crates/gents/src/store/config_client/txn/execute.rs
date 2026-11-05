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
    pub(crate) async fn execute_desired_state_mutation(
        &self,
        document: &str,
        variables: &Value,
        agent: bool,
        create: bool,
    ) -> Result<Value> {
        use super::super::client_signed_agent::{create_route, CreateRoute};
        let (intent, http) = match &self.backend {
            TxnBackend::Http { endpoint, .. } => (endpoint.client_signed_agent_creates, true),
            TxnBackend::Embedded { .. } => (false, false),
        };
        match create_route(intent, agent, create, http) {
            CreateRoute::Native => self.execute_with_variables(document, variables).await,
            CreateRoute::Refuse => Err(ConfigClientError::ClientAgentTransportRequired),
            CreateRoute::Signed => {
                let TxnBackend::Http { endpoint, id, .. } = &self.backend else {
                    return Err(ConfigClientError::ClientAgentTransportRequired);
                };
                let response =
                    super::super::client_signed_agent::execute(endpoint, id, document, variables)
                        .await?;
                self.affected_documents
                    .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
                #[cfg(test)]
                after_successful_mutation_for_test().await?;
                Ok(response)
            }
        }
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
        match &self.backend {
            TxnBackend::Embedded {
                node,
                handle,
                identity,
                native_operations,
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
            } => {
                let started = Instant::now();
                let node = Arc::clone(node);
                let request = QueryRequest::new(document)
                    .with_variables(variables.clone())
                    .with_identity(identity.clone());
                let handle = handle.clone();
                let result = execute_owned_native(
                    async move { node.execute_request_in_txn(request, &handle).await },
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
            after_successful_mutation_for_test().await?;
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
        if let TxnBackend::Http {
            endpoint,
            id,
            client,
        } = &self.backend
        {
            let actor = reader.ok_or(ConfigClientError::RelationshipActorRequired)?;
            if endpoint.is_anonymous() || endpoint.principal_did.as_deref() != Some(actor.as_str())
            {
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
                    .any(|(row, id)| row.doc_id != *id || (row.is_owner && !row.visible))
            {
                return Err(ConfigClientError::InvalidOwnerObservation);
            }
            return Ok(response
                .documents
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
                endpoint, client, ..
            } => {
                let url = format!(
                    "{}/collections?get_inactive=true",
                    graphql_api_base(endpoint.url())?
                );
                let versions: Value = endpoint
                    .authorize(client.get(&url))?
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
            node,
            handle,
            identity,
            native_operations,
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let started = Instant::now();
        let node = Arc::clone(node);
        let request = QueryRequest::new(document).with_identity(identity.clone());
        let handle = handle.clone();
        let result = execute_owned_native(
            async move { node.execute_request_in_txn(request, &handle).await },
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
            after_successful_mutation_for_test().await?;
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

fn native_relationship_error(error: defra_node::Error) -> ConfigClientError {
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

#[derive(serde::Deserialize)]
struct HttpOwnerObservation {
    doc_id: String,
    visible: bool,
    is_owner: bool,
}
