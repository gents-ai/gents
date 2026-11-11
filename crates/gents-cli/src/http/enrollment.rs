use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use gents::defra_node::Node as EmbeddedNode;
use gents_protocol::enrollment::{
    EnrollmentOfferRecord, EnrollmentOperatorAction, EnrollmentOperatorDecisionCommand,
    EnrollmentOperatorQueryCommand,
};
use gents_store::identity::NodeIdentity;
use serde::Serialize;
use tokio::sync::RwLock;

pub(crate) type EnrollmentOfferIssuerHandle =
    Arc<RwLock<Option<gents_p2p::p2p_reconcile::EnrollmentOfferIssuer>>>;
pub(crate) type EnrollmentDecisionServiceHandle = Arc<RwLock<Option<EnrollmentDecisionService>>>;

#[derive(Clone)]
pub(crate) struct EnrollmentDecisionService {
    identity: Arc<dyn NodeIdentity>,
    node: Arc<EmbeddedNode>,
    store: gents_p2p::p2p_reconcile::GraphqlEnrollmentStore,
}

impl EnrollmentDecisionService {
    pub(crate) fn new(identity: Arc<dyn NodeIdentity>, node: Arc<EmbeddedNode>) -> Self {
        Self {
            store: gents_p2p::p2p_reconcile::GraphqlEnrollmentStore::new(
                node.clone(),
                identity.clone(),
            ),
            identity,
            node,
        }
    }

    pub(crate) async fn decide(
        &self,
        command: &EnrollmentOperatorDecisionCommand,
    ) -> Result<gents_p2p::p2p_reconcile::EnrollmentDecisionOutcome> {
        let now = Utc::now();
        gents_p2p::p2p_reconcile::authenticate_operator_command(
            self.identity.as_ref(),
            command,
            now,
        )
        .await?;
        gents_p2p::p2p_reconcile::consume_operator_nonce(
            &self.node,
            &command.admin_did,
            &command.nonce,
            now,
        )
        .await?;
        match command.action {
            EnrollmentOperatorAction::Approve => {
                self.store
                    .decide_request_with_lease(
                        command.request_id.trim(),
                        gents_protocol::enrollment::EnrollmentDecisionKind::Approved,
                        std::time::Duration::from_secs(command.lease_seconds),
                    )
                    .await
            }
            EnrollmentOperatorAction::Deny => {
                self.store
                    .decide_request(
                        command.request_id.trim(),
                        gents_protocol::enrollment::EnrollmentDecisionKind::Denied,
                    )
                    .await
            }
            EnrollmentOperatorAction::Revoke => {
                self.store.revoke_request(command.request_id.trim()).await
            }
        }
    }

    pub(crate) async fn query(
        &self,
        command: &EnrollmentOperatorQueryCommand,
    ) -> Result<serde_json::Value> {
        let now = Utc::now();
        gents_p2p::p2p_reconcile::authenticate_operator_query(self.identity.as_ref(), command, now)
            .await?;
        gents_p2p::p2p_reconcile::consume_operator_nonce(
            &self.node,
            &command.admin_did,
            &command.nonce,
            now,
        )
        .await?;
        let projection = self.store.load_projection().await?;
        gents_p2p::p2p_reconcile::operator_query_view(projection, &command.query, now)
    }
}

impl EnrollmentDecisionService {
    pub(crate) async fn authenticate_node_reader(
        &self,
        signer_did: &str,
        payload: &[u8],
        sig: &[u8],
        nonce: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            self.identity.verify(signer_did, payload, sig).await?,
            "operator command signature is invalid"
        );
        let access = gents_store::config_client::ConfigAccess::local_as(
            self.node.clone(),
            identity::Did::new(signer_did)?,
        );
        let rendered = {
            use gents_protocol::document_query::{
                fields::node as fields, Filter, Query, Selection,
            };
            Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::NODE_DID, self.identity.did()))
                .render()
        };
        let response = access.read(&rendered).await?;
        anyhow::ensure!(
            response["data"]["Node"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
            "the client cannot read this Node"
        );
        gents_p2p::p2p_reconcile::consume_operator_nonce(&self.node, signer_did, nonce, Utc::now())
            .await
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum EnrollmentStatus {
    Available {
        token: String,
        offer: Box<EnrollmentOfferRecord>,
    },
    Unavailable {
        reason: &'static str,
    },
}

pub(crate) fn empty_issuer_handle() -> EnrollmentOfferIssuerHandle {
    Arc::new(RwLock::new(None))
}

pub(crate) fn empty_decision_service_handle() -> EnrollmentDecisionServiceHandle {
    Arc::new(RwLock::new(None))
}
