use std::sync::Arc;

use chrono::{DateTime, Utc};
use gents_protocol::{
    client_access::{
        admit_request, ClientAccessDecision, ClientAccessReceipt, ClientAccessRequest,
        ClientAccessState, DecideClientAccess, RequestAdmission,
    },
    document_query::{
        fields::{client_runtime_access_request as fields, node as node_fields},
        Filter, Input, Query, Selection, Update,
    },
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    config_client::{
        client_authentication::ClientAuthorization, ConfigAccess, ConfigClientError, NodePermission,
    },
    identity::{
        grant_enrollment_authority, load_enrollment_grant_receipts, EnrollmentGrantError,
        EnrollmentGrantWithdrawal, IdentityError, NodeIdentity,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum ClientAccessError {
    #[error("this client access request is not authorized")]
    Refused,
    #[error("the client access request expired before this runtime retained it")]
    ExpiredUnretained,
    #[error("the client access request has an invalid retained record")]
    Record,
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Did(#[from] identity::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Grant(#[from] EnrollmentGrantError),
}

impl crate::config_client::TransactionFailure for ClientAccessError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Grant(error) => error.config_client_error(),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct RetainedRequest {
    #[serde(rename = "_docID")]
    doc_id: String,
    intent_json: String,
    state: String,
    node_doc_id: String,
}

impl RetainedRequest {
    fn request(&self) -> Result<ClientAccessRequest, ClientAccessError> {
        Ok(serde_json::from_str(&self.intent_json)?)
    }

    fn receipt(&self) -> Result<ClientAccessReceipt, ClientAccessError> {
        let request = self.request()?;
        Ok(ClientAccessReceipt {
            request_id: request.request_id,
            node_did: request.node_did,
            client_did: request.client_did,
            state: serde_json::from_value(json!(self.state))?,
        })
    }
}

fn request_query(request_id: &str) -> gents_protocol::document_query::Rendered {
    Query::select(
        Selection::doc_id()
            .field(fields::INTENT_JSON)
            .field(fields::STATE)
            .field(fields::NODE_DOC_ID),
    )
    .filter(Filter::eq(fields::REQUEST_ID, request_id))
    .limit(2)
    .render()
}

fn retained(response: &serde_json::Value) -> Result<Option<RetainedRequest>, ClientAccessError> {
    let rows = response["data"]["ClientRuntimeAccessRequest"]
        .as_array()
        .ok_or(ClientAccessError::Record)?;
    if rows.len() > 1 {
        return Err(ClientAccessError::Record);
    }
    rows.first()
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}

pub struct ClientAccessOwner {
    node: Arc<defra_node::Node>,
    identity: Arc<dyn NodeIdentity>,
    decisions: Arc<tokio::sync::Mutex<()>>,
}

impl ClientAccessOwner {
    pub fn new(
        node: Arc<defra_node::Node>,
        identity: Arc<dyn NodeIdentity>,
        decisions: Arc<tokio::sync::Mutex<()>>,
    ) -> Self {
        Self {
            node,
            identity,
            decisions,
        }
    }

    fn runtime(&self) -> Result<ConfigAccess, ClientAccessError> {
        if self.node.node_identity_did() != Some(self.identity.did()) {
            return Err(ClientAccessError::Refused);
        }
        Ok(ConfigAccess::local_as(
            self.node.clone(),
            self.identity.did().parse()?,
        ))
    }

    async fn node_document(&self, runtime: &ConfigAccess) -> Result<String, ClientAccessError> {
        let query = Query::select(Selection::doc_id())
            .filter(Filter::eq(node_fields::NODE_DID, self.identity.did()))
            .limit(2)
            .render();
        let response = runtime.read(&query).await?;
        let Some([node]) = response["data"]["Node"].as_array().map(Vec::as_slice) else {
            return Err(ClientAccessError::Record);
        };
        node["_docID"]
            .as_str()
            .map(str::to_owned)
            .ok_or(ClientAccessError::Record)
    }

    pub async fn request(
        &self,
        caller: ClientAuthorization,
        request: ClientAccessRequest,
    ) -> Result<ClientAccessReceipt, ClientAccessError> {
        let _decision = self.decisions.lock().await;
        let runtime = self.runtime()?;
        let existing = retained(
            &runtime
                .read_for_decision(&request_query(&request.request_id))
                .await?,
        )?;
        let same = existing
            .as_ref()
            .map(|row| row.request())
            .transpose()?
            .as_ref()
            == Some(&request);
        let now = Utc::now();
        let (fresh, expired) = DateTime::parse_from_rfc3339(&request.issued_at)
            .ok()
            .zip(DateTime::parse_from_rfc3339(&request.expires_at).ok())
            .map(|(issued, expires)| {
                let valid = issued < expires && expires - issued <= chrono::Duration::minutes(5);
                (
                    valid && issued <= now && now < expires,
                    valid && expires <= now,
                )
            })
            .unwrap_or((false, false));
        let signature = crate::identity::verify_did_signature(
            &request.client_did,
            &request.signing_payload()?,
            &request.signature,
        )
        .unwrap_or(false);
        if request.request_id.is_empty() || request.label.trim().is_empty() {
            return Err(ClientAccessError::Refused);
        }
        match admit_request(
            caller.actor().as_str() == request.client_did,
            request.node_did == self.identity.did(),
            signature,
            existing.is_some(),
            same,
            fresh,
            expired,
        ) {
            RequestAdmission::Accepted => {}
            RequestAdmission::Refused => return Err(ClientAccessError::Refused),
            RequestAdmission::Expired => return Err(ClientAccessError::ExpiredUnretained),
        }
        let node_doc_id = self.node_document(&runtime).await?;
        if let Some(existing) = existing {
            if existing.node_doc_id != node_doc_id {
                return Err(ClientAccessError::Refused);
            }
            return existing.receipt();
        }
        let input = json!({
            "request_id": request.request_id, "node_did": request.node_did, "client_did": request.client_did,
            "intent_json": serde_json::to_string(&request)?, "state": "pending", "node_doc_id": node_doc_id, "parent_linked": true,
        });
        runtime.transact_write("client_access.request", |txn| {
            let input = &input;
            let node_doc_id = &node_doc_id;
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: ClientRuntimeAccessRequestMutationInputArg!) { create_ClientRuntimeAccessRequest(input: $input) { _docID } }",
                    &json!({"input": input}), gents_protocol::schemas::CLIENT_RUNTIME_ACCESS_REQUEST,
                ).await?;
                let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "ClientRuntimeAccessRequest").map_err(|_| ClientAccessError::Record)?;
                txn.add_resource_relationship("ClientRuntimeAccessRequest", &doc_id, "parent", "nodes", node_doc_id).await?;
                Ok::<_, ClientAccessError>(())
            })
        }).await?;
        Ok(ClientAccessReceipt {
            request_id: request.request_id,
            node_did: request.node_did,
            client_did: request.client_did,
            state: ClientAccessState::Pending,
        })
    }

    pub async fn decide(
        &self,
        caller: ClientAuthorization,
        decision: DecideClientAccess,
    ) -> Result<ClientAccessReceipt, ClientAccessError> {
        let _decision = self.decisions.lock().await;
        if decision.node_did != self.identity.did() {
            return Err(ClientAccessError::Refused);
        }
        let runtime = self.runtime()?;
        let approver = ConfigAccess::local_as(self.node.clone(), caller.actor().clone())
            .with_client_authorization(caller.clone())?;
        let existing = retained(
            &runtime
                .read_for_decision(&request_query(&decision.request_id))
                .await?,
        )?
        .ok_or(ClientAccessError::Record)?;
        let mut receipt = existing.receipt()?;
        let signed = existing.request()?;
        if receipt.request_id != decision.request_id
            || receipt.node_did != self.identity.did()
            || existing.node_doc_id != self.node_document(&runtime).await?
            || !crate::identity::verify_did_signature(
                &signed.client_did,
                &signed.signing_payload()?,
                &signed.signature,
            )?
        {
            return Err(ClientAccessError::Refused);
        }
        let allowed = approver
            .transact_readonly("client_access.approver", |txn| {
                let doc_id = &existing.node_doc_id;
                let actor = caller.actor();
                Box::pin(async move {
                    Ok::<_, ClientAccessError>(matches!(
                        txn.observe_document_access(
                            actor,
                            "Node",
                            std::slice::from_ref(doc_id),
                            defra_node::DocumentAccessMode::Document {
                                permission: acp::DocumentPermission::Update,
                                node_permission: NodePermission::DocumentUpdate,
                            }
                        )
                        .await?
                        .as_slice(),
                        [defra_node::DocumentAccessDecision::Allowed]
                    ))
                })
            })
            .await?;
        if !allowed {
            return Err(ClientAccessError::Refused);
        }
        let next = receipt.state.settle(decision.decision, true, true);
        if next == receipt.state {
            return Ok(receipt);
        }
        let member = receipt.client_did.parse()?;
        match decision.decision {
            ClientAccessDecision::Approve => {
                grant_enrollment_authority(
                    &approver,
                    &runtime,
                    &runtime,
                    &existing.node_doc_id,
                    &receipt.request_id,
                    &member,
                )
                .await?;
            }
            ClientAccessDecision::Reject | ClientAccessDecision::Revoke => {
                let receipts =
                    load_enrollment_grant_receipts(&runtime, &receipt.client_did).await?;
                if let Some(withdrawal) =
                    EnrollmentGrantWithdrawal::plan(&receipt.request_id, &receipts)?
                {
                    withdrawal
                        .withdraw_as(&approver, &runtime, &runtime, &existing.node_doc_id)
                        .await?;
                }
            }
        }
        let query = Update::doc(
            &existing.doc_id,
            Input::new().set(
                fields::STATE,
                serde_json::to_value(next)?
                    .as_str()
                    .ok_or(ClientAccessError::Record)?,
            ),
        )
        .render();
        let response = runtime
            .transact_write("client_access.settle", |txn| {
                let query = &query;
                Box::pin(async move { txn.run(query).await })
            })
            .await?;
        if gents_protocol::graphql::graphql_rows_from_response(
            &response,
            "update_ClientRuntimeAccessRequest",
        )
        .len()
            != 1
        {
            return Err(ClientAccessError::Record);
        }
        receipt.state = next;
        Ok(receipt)
    }
}

impl ClientAccessOwner {
    /// One owed-settlement pass's withdrawals for `caller`: the runtime acts
    /// as the caller under the credential its request carried and signs
    /// nothing (Lean `Identity.EnrollmentGrant.settlementStep`), serialized
    /// with this runtime's client-access decisions.
    pub async fn settle_withdrawals(
        &self,
        caller: ClientAuthorization,
    ) -> Result<Vec<gents_protocol::owed_settlement::OwedStanding>, ClientAccessError> {
        let _decision = self.decisions.lock().await;
        let runtime = self.runtime()?;
        let node_doc_id = self.node_document(&runtime).await?;
        let issuer = ConfigAccess::local_as(self.node.clone(), caller.actor().clone())
            .with_client_authorization(caller)?;
        Ok(crate::identity::settle_owed_withdrawals(&issuer, &runtime, &node_doc_id).await?)
    }

    /// The administration removals this home owes that `caller` may
    /// complete, for a client to show its person.
    pub async fn owed_removals(
        &self,
        caller: ClientAuthorization,
    ) -> Result<Vec<gents_protocol::owed_settlement::OwedRemoval>, ClientAccessError> {
        let runtime = self.runtime()?;
        let node_doc_id = self.node_document(&runtime).await?;
        let observer = ConfigAccess::local_as(self.node.clone(), caller.actor().clone())
            .with_client_authorization(caller)?;
        Ok(crate::identity::owed_removals(&observer, &runtime, &node_doc_id).await?)
    }
}

pub async fn list_requests(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Vec<ClientAccessReceipt>, ClientAccessError> {
    let query = Query::select(
        Selection::doc_id()
            .field(fields::INTENT_JSON)
            .field(fields::STATE)
            .field(fields::NODE_DOC_ID),
    )
    .filter(Filter::eq(fields::NODE_DID, node_did))
    .render();
    let response = access.read(&query).await?;
    let retained: Vec<RetainedRequest> =
        serde_json::from_value(response["data"]["ClientRuntimeAccessRequest"].clone())?;
    retained
        .iter()
        .map(|row| {
            let request = row.request()?;
            if request.node_did != node_did
                || !crate::identity::verify_did_signature(
                    &request.client_did,
                    &request.signing_payload()?,
                    &request.signature,
                )
                .unwrap_or(false)
            {
                return Err(ClientAccessError::Record);
            }
            row.receipt()
        })
        .collect()
}

#[cfg(test)]
mod conformance;

#[cfg(test)]
mod native;
