pub const ENROLLMENT_PROTOCOL_VERSION: u8 = 2;
pub const DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS: u64 = 90 * 24 * 60 * 60;
pub const ENROLLMENT_DIGEST_DOMAIN: &str = "gents-enrollment-request-v1";
pub const ENROLLMENT_DIGEST_PREFIX: &str = "utf8hex-v1:";
const OFFER_SIGNATURE_DOMAIN: &str = "gents-enrollment-offer-signature-v1";
const REQUEST_SIGNATURE_DOMAIN: &str = "gents-enrollment-request-signature-v1";
const DECISION_SIGNATURE_DOMAIN: &str = "gents-enrollment-decision-signature-v1";
const REVISION_SIGNATURE_DOMAIN: &str = "gents-network-authorization-signature-v1";
const ROUTE_RECEIPT_SIGNATURE_DOMAIN: &str = "gents-enrollment-route-receipt-signature-v1";
const OPERATOR_DECISION_COMMAND_SIGNATURE_DOMAIN: &str =
    "gents-enrollment-operator-decision-command-v1";
const OPERATOR_QUERY_COMMAND_SIGNATURE_DOMAIN: &str = "gents-enrollment-operator-query-command-v1";
const MAX_OFFER_TOKEN_BYTES: usize = 32 * 1024;
const MAX_ENROLLMENT_FIELD_BYTES: usize = 16 * 1024;
const ENROLLMENT_SCHEMA_DOMAIN: &str = "gents-enrollment-schema-v1";

mod canonical;
mod decision;
mod error;
mod offer;
mod operator;
mod request;
mod revision;
mod route_receipt;

pub use canonical::{
    authorization_lease_is_fresh_at, canonical_domain_payload, canonical_enrollment_digest,
    canonical_enrollment_payload, derive_enrollment_id, enrollment_schema_fingerprint,
    frame_enrollment_field,
};
pub use decision::{
    derive_decision_id, EnrollmentDecisionKind, EnrollmentDecisionRecord,
    EnrollmentDecisionRefusalCode,
};
pub use error::EnrollmentError;
pub use offer::{decode_offer, encode_offer, EnrollmentOfferRecord};
pub use operator::{
    EnrollmentOperatorAction, EnrollmentOperatorDecisionCommand, EnrollmentOperatorQuery,
    EnrollmentOperatorQueryCommand,
};
pub use request::EnrollmentRequestRecord;
pub use revision::{derive_revision_id, AuthorizationRevisionKind, AuthorizationRevisionRecord};
pub use route_receipt::{
    derive_route_receipt_id, EnrollmentRouteReceiptDirection, EnrollmentRouteReceiptRecord,
};
