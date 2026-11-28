//! The node calls Gents makes under a request context, each scoped as
//! [`scoped`](super::scoped) scopes it.

use std::future::Future;

use defra_core::{RequestContext, TxnId};
use defra_node::{Node, TransactionHandle};
use identity::Did;

use super::{acting_for, handle, node_transaction, scoped, transaction, transaction_error};

/// The node calls Gents makes under a request context, each scoped as
/// [`scoped`] scopes it.
pub trait ContextCalls {
    fn add_dac_policy_as(
        &self,
        context: &RequestContext,
        policy: &str,
    ) -> impl Future<Output = defra_node::Result<String>> + Send;
    fn add_dac_policy_with_counter_as(
        &self,
        context: &RequestContext,
        policy: &str,
        counter: u64,
    ) -> impl Future<Output = defra_node::Result<String>> + Send;
    fn get_dac_policy_as(
        &self,
        context: &RequestContext,
        policy_id: &str,
    ) -> impl Future<Output = defra_node::Result<Option<acp::Policy>>> + Send;
    fn add_nac_actor_relationship_as(
        &self,
        context: &RequestContext,
        relation: &str,
        target_actor: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn delete_nac_actor_relationship_as(
        &self,
        context: &RequestContext,
        relation: &str,
        target_actor: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn explain_nac_subject_as(
        &self,
        context: &RequestContext,
        subject: &str,
    ) -> impl Future<Output = defra_node::Result<Vec<acp::nac::PermissionProvenance>>> + Send;
    fn disable_nac_as(
        &self,
        context: &RequestContext,
    ) -> impl Future<Output = defra_node::Result<()>> + Send;
    fn add_dac_actor_relationship_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn delete_dac_actor_relationship_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn observe_dac_actor_relationship_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn list_dac_relationships_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
    ) -> impl Future<Output = defra_node::Result<Vec<acp::DocumentRelationship>>> + Send;
    fn owed_acp_as(
        &self,
        context: &RequestContext,
        actor: &str,
    ) -> impl Future<Output = defra_node::Result<Vec<acp::OwedEntry>>> + Send;
    fn await_owed_acp_as(
        &self,
        context: &RequestContext,
        handle: &TransactionHandle,
        deadline: std::time::Duration,
    ) -> impl Future<Output = defra_node::Result<acp::OwedWait>> + Send;
    fn reauthorize_owed_acp_as(
        &self,
        context: &RequestContext,
        entry: &acp::OwedRef,
        credential: defra_node::SignerCredential,
    ) -> impl Future<Output = defra_node::Result<acp::OwedOutcome>> + Send;
    fn certified_assignments_as(
        &self,
        context: &RequestContext,
        query: acp::AssignmentQuery,
    ) -> impl Future<Output = defra_node::Result<acp::CertifiedAssignments>> + Send;
    fn acp_light_client_status_as(
        &self,
        context: &RequestContext,
    ) -> impl Future<Output = defra_node::Result<acp::LightClientStatus>> + Send;
    fn get_collections_as(
        &self,
        context: &RequestContext,
        selector: &schema::CollectionSelector,
    ) -> impl Future<Output = defra_node::Result<Vec<schema::CollectionVersion>>> + Send;
    fn add_collection_as(
        &self,
        context: &RequestContext,
        sdl: &str,
    ) -> impl Future<Output = defra_node::Result<Vec<schema::CollectionVersion>>> + Send;
    fn patch_collection_as(
        &self,
        context: &RequestContext,
        patch: &str,
        migration: Option<&serde_json::Value>,
    ) -> impl Future<Output = defra_node::Result<()>> + Send;
    fn set_active_collection_version_as(
        &self,
        context: &RequestContext,
        version_id: &str,
    ) -> impl Future<Output = defra_node::Result<()>> + Send;
    fn set_migration_as(
        &self,
        context: &RequestContext,
        config: &serde_json::Value,
    ) -> impl Future<Output = defra_node::Result<String>> + Send;
    fn add_view_as(
        &self,
        context: &RequestContext,
        source_query: &str,
        target_sdl: &str,
        transform: Option<&str>,
    ) -> impl Future<Output = defra_node::Result<Vec<schema::CollectionVersion>>> + Send;
    fn materialize_collection_as(
        &self,
        context: &RequestContext,
        collection_name: &str,
    ) -> impl Future<Output = defra_node::Result<usize>> + Send;
    fn truncate_collection_as(
        &self,
        context: &RequestContext,
        name: &str,
        filter: Option<&query::documents::Filter>,
    ) -> impl Future<Output = defra_node::Result<()>> + Send;
    fn restore_shared_transport_collection_as(
        &self,
        context: &RequestContext,
        sdl: &str,
        selection: acp::TransportSentinelQuery,
    ) -> impl Future<Output = defra_node::Result<Vec<schema::CollectionVersion>>> + Send;
    fn execute_as(
        &self,
        context: &RequestContext,
        query: &str,
    ) -> impl Future<Output = defra_node::QueryResponse> + Send;
    fn execute_request_as(
        &self,
        context: &RequestContext,
        request: defra_node::QueryRequest,
    ) -> impl Future<Output = defra_node::Result<defra_node::QueryResponse>> + Send;
    fn execute_request_in_txn(
        &self,
        request: defra_node::QueryRequest,
        handle: &TransactionHandle,
    ) -> impl Future<Output = defra_node::QueryResponse> + Send;
    fn execute_verified_create_in_txn(
        &self,
        request: defra_node::QueryRequest,
        handle: &TransactionHandle,
        graph: &db::block::builder::VerifiedClientCreate,
        credential: defra_node::SignerCredential,
    ) -> impl Future<Output = defra_node::QueryResponse> + Send;
    fn execute_verified_mutation_in_txn(
        &self,
        request: defra_node::QueryRequest,
        handle: &TransactionHandle,
        graph: &db::block::builder::VerifiedClientMutation,
        credential: defra_node::SignerCredential,
    ) -> impl Future<Output = defra_node::QueryResponse> + Send;
    fn add_dac_actor_relationship_for_in_txn(
        &self,
        handle: &TransactionHandle,
        actor_context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn delete_dac_actor_relationship_for_in_txn(
        &self,
        handle: &TransactionHandle,
        actor_context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn change_nac_source_permission_for_in_txn(
        &self,
        handle: &TransactionHandle,
        actor_context: &RequestContext,
        edge: &query::txn::SourcePermission,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn observe_document_access_in_txn(
        &self,
        handle: &TransactionHandle,
        collection: &str,
        doc_ids: &[String],
        actor: Did,
        mode: query::owners::DocumentAccessMode,
    ) -> impl Future<Output = query::error::Result<Vec<query::owners::DocumentAccessDecision>>> + Send;
    fn observe_document_owners_in_txn(
        &self,
        handle: &TransactionHandle,
        collection: &str,
        doc_ids: &[String],
        identity: Option<Did>,
    ) -> impl Future<Output = query::error::Result<Vec<query::owners::DocumentOwnerObservation>>> + Send;
    fn authorized_signed_block_bytes_by(
        &self,
        cid: &str,
        reader: &str,
    ) -> impl Future<Output = defra_node::Result<(Vec<u8>, Vec<u8>)>> + Send;
    fn add_dac_actor_relationship_by(
        &self,
        actor: &str,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn delete_dac_actor_relationship_by(
        &self,
        actor: &str,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> impl Future<Output = defra_node::Result<bool>> + Send;
    fn begin_transaction(
        &self,
        read_only: bool,
    ) -> impl Future<Output = Result<TransactionHandle, query::TransactionError>> + Send;
    fn commit_transaction(
        &self,
        handle: &TransactionHandle,
    ) -> impl Future<Output = Result<(), query::TransactionError>> + Send;
    fn discard_transaction(
        &self,
        handle: &TransactionHandle,
    ) -> impl Future<Output = Result<(), query::TransactionError>> + Send;
}

impl ContextCalls for Node {
    async fn add_dac_policy_as(
        &self,
        context: &RequestContext,
        policy: &str,
    ) -> defra_node::Result<String> {
        scoped(self, context)?.add_dac_policy(policy, None).await
    }
    async fn add_dac_policy_with_counter_as(
        &self,
        context: &RequestContext,
        policy: &str,
        counter: u64,
    ) -> defra_node::Result<String> {
        scoped(self, context)?
            .add_dac_policy_with_counter(policy, counter)
            .await
    }
    async fn get_dac_policy_as(
        &self,
        context: &RequestContext,
        policy_id: &str,
    ) -> defra_node::Result<Option<acp::Policy>> {
        scoped(self, context)?.get_dac_policy(policy_id).await
    }
    async fn add_nac_actor_relationship_as(
        &self,
        context: &RequestContext,
        relation: &str,
        target_actor: &str,
    ) -> defra_node::Result<bool> {
        scoped(self, context)?
            .add_nac_actor_relationship(relation, target_actor)
            .await
    }
    async fn delete_nac_actor_relationship_as(
        &self,
        context: &RequestContext,
        relation: &str,
        target_actor: &str,
    ) -> defra_node::Result<bool> {
        scoped(self, context)?
            .delete_nac_actor_relationship(relation, target_actor)
            .await
    }
    async fn explain_nac_subject_as(
        &self,
        context: &RequestContext,
        subject: &str,
    ) -> defra_node::Result<Vec<acp::nac::PermissionProvenance>> {
        scoped(self, context)?.explain_nac_subject(subject).await
    }
    async fn disable_nac_as(&self, context: &RequestContext) -> defra_node::Result<()> {
        scoped(self, context)?.disable_nac().await
    }
    async fn add_dac_actor_relationship_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        scoped(self, context)?
            .add_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn delete_dac_actor_relationship_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        scoped(self, context)?
            .delete_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn observe_dac_actor_relationship_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        scoped(self, context)?
            .observe_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn list_dac_relationships_as(
        &self,
        context: &RequestContext,
        collection: &str,
        doc_id: &str,
    ) -> defra_node::Result<Vec<acp::DocumentRelationship>> {
        scoped(self, context)?
            .list_dac_relationships(collection, doc_id)
            .await
    }
    async fn owed_acp_as(
        &self,
        context: &RequestContext,
        actor: &str,
    ) -> defra_node::Result<Vec<acp::OwedEntry>> {
        scoped(self, context)?.owed_acp(actor).await
    }
    async fn await_owed_acp_as(
        &self,
        context: &RequestContext,
        handle: &TransactionHandle,
        deadline: std::time::Duration,
    ) -> defra_node::Result<acp::OwedWait> {
        scoped(self, context)?
            .await_owed_acp(transaction(handle)?, deadline)
            .await
    }
    async fn reauthorize_owed_acp_as(
        &self,
        context: &RequestContext,
        entry: &acp::OwedRef,
        credential: defra_node::SignerCredential,
    ) -> defra_node::Result<acp::OwedOutcome> {
        scoped(self, context)?
            .reauthorize_owed_acp(entry, credential)
            .await
    }
    async fn certified_assignments_as(
        &self,
        context: &RequestContext,
        query: acp::AssignmentQuery,
    ) -> defra_node::Result<acp::CertifiedAssignments> {
        scoped(self, context)?.certified_assignments(query).await
    }
    async fn acp_light_client_status_as(
        &self,
        context: &RequestContext,
    ) -> defra_node::Result<acp::LightClientStatus> {
        scoped(self, context)?.acp_light_client_status().await
    }
    async fn get_collections_as(
        &self,
        context: &RequestContext,
        selector: &schema::CollectionSelector,
    ) -> defra_node::Result<Vec<schema::CollectionVersion>> {
        scoped(self, context)?.get_collections(selector).await
    }
    async fn add_collection_as(
        &self,
        context: &RequestContext,
        sdl: &str,
    ) -> defra_node::Result<Vec<schema::CollectionVersion>> {
        scoped(self, context)?.add_collection(sdl).await
    }
    async fn patch_collection_as(
        &self,
        context: &RequestContext,
        patch: &str,
        migration: Option<&serde_json::Value>,
    ) -> defra_node::Result<()> {
        scoped(self, context)?
            .patch_collection(patch, migration)
            .await
    }
    async fn set_active_collection_version_as(
        &self,
        context: &RequestContext,
        version_id: &str,
    ) -> defra_node::Result<()> {
        scoped(self, context)?
            .set_active_collection_version(version_id)
            .await
    }
    async fn set_migration_as(
        &self,
        context: &RequestContext,
        config: &serde_json::Value,
    ) -> defra_node::Result<String> {
        scoped(self, context)?.set_migration(config).await
    }
    async fn add_view_as(
        &self,
        context: &RequestContext,
        source_query: &str,
        target_sdl: &str,
        transform: Option<&str>,
    ) -> defra_node::Result<Vec<schema::CollectionVersion>> {
        scoped(self, context)?
            .add_view(source_query, target_sdl, transform)
            .await
    }
    async fn materialize_collection_as(
        &self,
        context: &RequestContext,
        collection_name: &str,
    ) -> defra_node::Result<usize> {
        scoped(self, context)?
            .materialize_collection(collection_name)
            .await
    }
    async fn truncate_collection_as(
        &self,
        context: &RequestContext,
        name: &str,
        filter: Option<&query::documents::Filter>,
    ) -> defra_node::Result<()> {
        scoped(self, context)?
            .truncate_collection(name, filter)
            .await
    }
    async fn restore_shared_transport_collection_as(
        &self,
        context: &RequestContext,
        sdl: &str,
        selection: acp::TransportSentinelQuery,
    ) -> defra_node::Result<Vec<schema::CollectionVersion>> {
        scoped(self, context)?
            .restore_shared_transport_collection(sdl, selection)
            .await
    }
    async fn execute_as(&self, context: &RequestContext, query: &str) -> defra_node::QueryResponse {
        match scoped(self, context) {
            Ok(scope) => scope.execute(query).await,
            Err(error) => refusal(error),
        }
    }
    async fn execute_request_as(
        &self,
        context: &RequestContext,
        request: defra_node::QueryRequest,
    ) -> defra_node::Result<defra_node::QueryResponse> {
        Ok(scoped(self, context)?.execute_request(request).await)
    }
    async fn execute_request_in_txn(
        &self,
        request: defra_node::QueryRequest,
        handle: &TransactionHandle,
    ) -> defra_node::QueryResponse {
        match transaction(handle) {
            Ok(id) => self.with_transaction(id).execute_request(request).await,
            Err(error) => refusal(error),
        }
    }
    async fn execute_verified_create_in_txn(
        &self,
        request: defra_node::QueryRequest,
        handle: &TransactionHandle,
        graph: &db::block::builder::VerifiedClientCreate,
        credential: defra_node::SignerCredential,
    ) -> defra_node::QueryResponse {
        match transaction(handle) {
            Ok(id) => {
                self.with_transaction(id)
                    .execute_verified_create(request, graph, credential)
                    .await
            }
            Err(error) => refusal(error),
        }
    }
    async fn execute_verified_mutation_in_txn(
        &self,
        request: defra_node::QueryRequest,
        handle: &TransactionHandle,
        graph: &db::block::builder::VerifiedClientMutation,
        credential: defra_node::SignerCredential,
    ) -> defra_node::QueryResponse {
        match transaction(handle) {
            Ok(id) => {
                self.with_transaction(id)
                    .execute_verified_mutation(request, graph, credential)
                    .await
            }
            Err(error) => refusal(error),
        }
    }
    async fn add_dac_actor_relationship_for_in_txn(
        &self,
        handle: &TransactionHandle,
        actor_context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        acting_for(self, actor_context, handle)?
            .add_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn delete_dac_actor_relationship_for_in_txn(
        &self,
        handle: &TransactionHandle,
        actor_context: &RequestContext,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        acting_for(self, actor_context, handle)?
            .delete_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn change_nac_source_permission_for_in_txn(
        &self,
        handle: &TransactionHandle,
        actor_context: &RequestContext,
        edge: &query::txn::SourcePermission,
    ) -> defra_node::Result<bool> {
        acting_for(self, actor_context, handle)?
            .change_nac_source_permission(edge)
            .await
    }
    async fn observe_document_access_in_txn(
        &self,
        handle: &TransactionHandle,
        collection: &str,
        doc_ids: &[String],
        actor: Did,
        mode: query::owners::DocumentAccessMode,
    ) -> query::error::Result<Vec<query::owners::DocumentAccessDecision>> {
        self.with_transaction(query_transaction(handle)?)
            .observe_document_access(collection, doc_ids, actor, mode)
            .await
    }
    async fn observe_document_owners_in_txn(
        &self,
        handle: &TransactionHandle,
        collection: &str,
        doc_ids: &[String],
        identity: Option<Did>,
    ) -> query::error::Result<Vec<query::owners::DocumentOwnerObservation>> {
        self.with_transaction(query_transaction(handle)?)
            .observe_document_owners(collection, doc_ids, identity)
            .await
    }
    async fn authorized_signed_block_bytes_by(
        &self,
        cid: &str,
        reader: &str,
    ) -> defra_node::Result<(Vec<u8>, Vec<u8>)> {
        self.with_identity(Did::new(reader)?)
            .authorized_signed_block_bytes(cid)
            .await
    }
    async fn add_dac_actor_relationship_by(
        &self,
        actor: &str,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        self.with_identity(Did::new(actor)?)
            .add_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn delete_dac_actor_relationship_by(
        &self,
        actor: &str,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
    ) -> defra_node::Result<bool> {
        self.with_identity(Did::new(actor)?)
            .delete_dac_actor_relationship(collection, doc_id, relation, target)
            .await
    }
    async fn begin_transaction(
        &self,
        read_only: bool,
    ) -> Result<TransactionHandle, query::TransactionError> {
        let transaction = self.new_txn(read_only).await.map_err(transaction_error)?;
        Ok(handle(transaction.detach()))
    }
    async fn commit_transaction(
        &self,
        handle: &TransactionHandle,
    ) -> Result<(), query::TransactionError> {
        node_transaction(self, handle)
            .map_err(transaction_error)?
            .commit()
            .await
            .map_err(|error| finish_transaction_error(handle, error))
    }
    async fn discard_transaction(
        &self,
        handle: &TransactionHandle,
    ) -> Result<(), query::TransactionError> {
        node_transaction(self, handle)
            .map_err(transaction_error)?
            .discard()
            .await
            .map_err(|error| finish_transaction_error(handle, error))
    }
}

fn finish_transaction_error(
    handle: &TransactionHandle,
    error: defra_node::Error,
) -> query::TransactionError {
    match error {
        defra_node::Error::TransactionNotFound => {
            query::TransactionError::NotFound(handle.as_str().to_owned())
        }
        error => transaction_error(error),
    }
}

fn query_transaction(handle: &TransactionHandle) -> query::error::Result<TxnId> {
    handle.as_str().parse().map_err(|_| {
        query::error::QueryError::internal(format!("transaction ID '{handle}' is not a number"))
    })
}

fn refusal(error: defra_node::Error) -> defra_node::QueryResponse {
    defra_node::QueryResponse::error(error.to_string())
}
