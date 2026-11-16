use gents_protocol::document_query::{fields, Filter, Query, Selection};
use gents_protocol::views::View;
use gents_store::config_client::ConfigAccess;
use gents_store::views::ViewError;
use serde::Deserialize;

use crate::p2p_reconcile::enrollment_rows::{
    EnrollmentDecisionRow, EnrollmentPinRow, EnrollmentRequestRow, EnrollmentRevisionRow,
};

pub struct EnrollmentPins;

impl View for EnrollmentPins {
    type Collection = fields::network_admin_pin::Doc;
    type Row = EnrollmentPinRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(
            Selection::new()
                .field(fields::network_admin_pin::NETWORK_ID)
                .field(fields::network_admin_pin::ADMIN_DID),
        )
    }
}

pub struct EnrollmentDecisions;

impl View for EnrollmentDecisions {
    type Collection = fields::network_enrollment_decision::Doc;
    type Row = EnrollmentDecisionRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(
            Selection::new()
                .field(fields::network_enrollment_decision::PROTOCOL_VERSION)
                .field(fields::network_enrollment_decision::DECISION_ID)
                .field(fields::network_enrollment_decision::REQUEST_ID)
                .field(fields::network_enrollment_decision::REQUEST_DIGEST)
                .field(fields::network_enrollment_decision::NETWORK_ID)
                .field(fields::network_enrollment_decision::ADMIN_DID)
                .field(fields::network_enrollment_decision::CANDIDATE_DID)
                .field(fields::network_enrollment_decision::CANDIDATE_PEER)
                .field(fields::network_enrollment_decision::OWNER_NODE_DID)
                .field(fields::network_enrollment_decision::DECISION)
                .field(fields::network_enrollment_decision::AUTHORIZATION_SEQUENCE)
                .field(fields::network_enrollment_decision::AUTHORIZATION_EXPIRES_AT)
                .field(fields::network_enrollment_decision::DECIDED_AT)
                .field(fields::network_enrollment_decision::SIGNER_DID)
                .field(fields::network_enrollment_decision::ADMIN_SIG),
        )
    }
}

pub struct EnrollmentRevisions;

impl View for EnrollmentRevisions {
    type Collection = fields::network_authorization_revision::Doc;
    type Row = RevisionCandidate;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(
            Selection::new()
                .field(fields::network_authorization_revision::PROTOCOL_VERSION)
                .field(fields::network_authorization_revision::REVISION_ID)
                .field(fields::network_authorization_revision::REQUEST_ID)
                .field(fields::network_authorization_revision::REQUEST_DIGEST)
                .field(fields::network_authorization_revision::NETWORK_ID)
                .field(fields::network_authorization_revision::ADMIN_DID)
                .field(fields::network_authorization_revision::MEMBER_DID)
                .field(fields::network_authorization_revision::MEMBER_PEER)
                .field(fields::network_authorization_revision::OWNER_NODE_DID)
                .field(fields::network_authorization_revision::SEQUENCE)
                .field(fields::network_authorization_revision::AUTHORIZATION_EXPIRES_AT)
                .field(fields::network_authorization_revision::KIND)
                .field(fields::network_authorization_revision::ISSUED_AT)
                .field(fields::network_authorization_revision::SIGNER_DID)
                .field(fields::network_authorization_revision::ADMIN_SIG),
        )
    }
}

#[derive(Default)]
pub struct EnrollmentRequests {
    pub request_id: Option<String>,
    pub offer_id: Option<String>,
}

impl View for EnrollmentRequests {
    type Collection = fields::network_enrollment_request::Doc;
    type Row = EnrollmentRequestRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(fields::network_enrollment_request::DOC_ID)
                .field(fields::network_enrollment_request::PROTOCOL_VERSION)
                .field(fields::network_enrollment_request::REQUEST_ID)
                .field(fields::network_enrollment_request::REQUEST_DIGEST)
                .field(fields::network_enrollment_request::OFFER_ID)
                .field(fields::network_enrollment_request::OFFER_TOKEN)
                .field(fields::network_enrollment_request::CHALLENGE)
                .field(fields::network_enrollment_request::NETWORK_ID)
                .field(fields::network_enrollment_request::ADMIN_DID)
                .field(fields::network_enrollment_request::SERVER_PEER)
                .field(fields::network_enrollment_request::CANDIDATE_DID)
                .field(fields::network_enrollment_request::CANDIDATE_PEER)
                .field(fields::network_enrollment_request::CANDIDATE_NAME)
                .field(fields::network_enrollment_request::CANDIDATE_TICKET)
                .field(fields::network_enrollment_request::OWNER_NODE_DID)
                .field(fields::network_enrollment_request::PROFILE)
                .field(fields::network_enrollment_request::CLIENT_NONCE)
                .field(fields::network_enrollment_request::ISSUED_AT)
                .field(fields::network_enrollment_request::EXPIRES_AT)
                .field(fields::network_enrollment_request::CANDIDATE_SIG),
        );
        if let Some(id) = &self.request_id {
            query = query.filter(Filter::eq(
                fields::network_enrollment_request::REQUEST_ID,
                id,
            ));
        }
        if let Some(id) = &self.offer_id {
            query = query.filter(Filter::eq(fields::network_enrollment_request::OFFER_ID, id));
        }
        query
    }
}

pub struct RevisionCandidate(Option<EnrollmentRevisionRow>);

impl<'de> Deserialize<'de> for RevisionCandidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self(serde_json::from_value(value).ok()))
    }
}

pub struct EnrollmentListing {
    pub pins: Vec<EnrollmentPinRow>,
    pub requests: Vec<EnrollmentRequestRow>,
    pub decisions: Vec<EnrollmentDecisionRow>,
    pub revisions: Vec<EnrollmentRevisionRow>,
}

pub async fn read_enrollment_listing(
    access: &ConfigAccess,
) -> Result<EnrollmentListing, ViewError> {
    access
        .transact_readonly("view.enrollment_listing", |txn| {
            Box::pin(async move {
                let pins = txn.read_view(&EnrollmentPins).await?;
                let requests = txn.read_view(&EnrollmentRequests::default()).await?;
                let decisions = txn.read_view(&EnrollmentDecisions).await?;
                let revisions = txn
                    .read_view(&EnrollmentRevisions)
                    .await?
                    .into_iter()
                    .filter_map(|candidate| candidate.0)
                    .filter(|row| row.to_record().is_ok())
                    .collect();
                Ok(EnrollmentListing {
                    pins,
                    requests,
                    decisions,
                    revisions,
                })
            })
        })
        .await
}
