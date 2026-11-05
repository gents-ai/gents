use std::sync::Arc;

use gents_protocol::request_admission::AgentRequestCreate;
use serde_json::json;

use super::apply_txn::TxnBackend;
use super::native::execute_owned_native;
use super::{ConfigApplyTxn, ConfigClientError, Result};

impl ConfigApplyTxn<'_> {
    /// The verified request and its user-owned relationships are staged with
    /// the selected Agent's queue update in the same native transaction.
    pub(crate) async fn stage_verified_edit_request(
        &self,
        create: &AgentRequestCreate,
        graph: Arc<db::block::builder::VerifiedClientCreate>,
        session_doc: &str,
        agent_doc: &str,
    ) -> Result<String> {
        let TxnBackend::Embedded {
            node,
            handle,
            identity,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::TransactionAuthorizationUnsupported);
        };
        if self.managed_agent_signer().is_none()
            || identity.as_ref().map(identity::Did::as_str) != Some(create.agent_did.as_str())
            || graph.signer_did() != create.admission.signer_did
            || create.session_doc_id.as_deref() != Some(session_doc)
            || create.agent_doc_id.as_deref() != Some(agent_doc)
        {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let input = crate::request::lifecycle::client_request_input(create)
            .map_err(|_| ConfigClientError::ClientConfigurationUpdateInvalid)?;
        if !graph.matches_input("AgentRequest", &input) {
            return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
        }
        let signer = identity::Did::new(graph.signer_did())
            .map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let expected = graph.doc_id().to_owned();
        let response = self.execute_verified_client_create(
            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
            &json!({"input": input}), graph,
        ).await?;
        let created = crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
        if created != expected {
            return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
        }
        let context = defra_core::RequestContext::default().with_identity(Some(signer.to_string()));
        let relations = [
            (
                "parent",
                super::super::relationships::resource_target("agents", agent_doc)?,
            ),
            (
                "session",
                super::super::relationships::resource_target("sessions", session_doc)?,
            ),
            ("working_agent", create.agent_did.clone()),
        ];
        for (relation, target) in relations {
            let node = Arc::clone(node);
            let handle = handle.clone();
            let context = context.clone();
            let doc = created.clone();
            execute_owned_native(
                async move {
                    node.add_dac_actor_relationship_for_in_txn(
                        &handle,
                        &context,
                        "AgentRequest",
                        &doc,
                        relation,
                        &target,
                    )
                    .await
                },
                Arc::clone(native_operations),
                self.operation,
            )
            .await?
            .map_err(super::execute::native_relationship_error)?;
        }
        Ok(created)
    }
}
