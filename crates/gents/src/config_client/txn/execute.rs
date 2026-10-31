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
use crate::config_client::write_telemetry::{record_slow_native_phase, ConflictSource};

impl ConfigApplyTxn<'_> {
    /// Execute exactly once inside this snapshot; conflict replay is the outer `transact` owner's job.
    pub async fn execute(&self, document: &str) -> Result<Value> {
        self.execute_with_variables(document, &json!({})).await
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
                crate::graphql::into_data_envelope(response.data)
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
    /// HTTP transactions lack this native capability and are refused explicitly.
    pub async fn document_owners(
        &self,
        collection: &str,
        doc_ids: &[String],
    ) -> Result<Vec<query::owners::DocumentOwnerObservation>> {
        let TxnBackend::Embedded { identity, .. } = &self.backend else {
            return Err(ConfigClientError::OwnerObservationRequiresEmbedded);
        };
        self.observe_document_owners(collection, doc_ids, identity.clone())
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
        crate::graphql::validate_collection_identifier(collection)?;
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
        .map_err(|error| match error {
            query::error::QueryError::TransactionConflict(_) => {
                retry::transaction_conflict(ConflictSource::TypedError)
            }
            error => retry::transaction_storage_failure(error.into()),
        })
    }

    /// Check exact document visibility for a delegated actor in this node-owned
    /// transaction; native document ACP and pending registrations stay bound to it.
    pub async fn document_visibility_as(
        &self,
        actor: identity::Did,
        collection: &str,
        doc_ids: &[String],
    ) -> Result<Vec<query::owners::DocumentOwnerObservation>> {
        crate::graphql::validate_collection_identifier(collection)?;
        let TxnBackend::Embedded {
            node,
            handle,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(super::ConfigClientError::OwnerObservationRequiresEmbedded);
        };
        let node = Arc::clone(node);
        let handle = handle.clone();
        let collection = collection.to_owned();
        let doc_ids = doc_ids.to_vec();
        execute_owned_native(
            async move {
                node.observe_document_visibility_in_txn(&handle, &collection, &doc_ids, actor)
                    .await
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(|error| match error {
            query::error::QueryError::TransactionConflict(_) => {
                retry::transaction_conflict(ConflictSource::TypedError)
            }
            error => retry::transaction_storage_failure(error.into()),
        })
    }

    pub async fn collection_version(&self, collection: &str) -> Result<Option<Value>> {
        crate::graphql::validate_collection_identifier(collection)?;
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
                crate::graphql::mutation_affected_documents(&response) as u64,
                Ordering::Relaxed,
            );
            #[cfg(test)]
            after_successful_mutation_for_test().await?;
        }
        Ok(response)
    }
}
