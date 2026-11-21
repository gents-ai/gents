use gents_lean_vocab::families::identity_genesis_read_access_receiver_access as family;
use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};
use serde::{Deserialize, Serialize};

use super::{receiver_genesis_access, GenesisReadAccessError};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Collection {
    PeerRegistry,
    NetworkEnrollmentRequest,
    NetworkEnrollmentDecision,
    NetworkAuthorizationRevision,
    NetworkEnrollmentRouteReceipt,
    EnrollmentRenewalIntent,
    EnrollmentRenewalOffer,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Resource {
    Discovery,
    EnrollmentRecords,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Binding {
    HomeHousehold,
    OtherPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Access {
    None,
    Everyone,
    AddressedReceiver,
    PolicyMismatch,
}

fn collection(collection: Collection) -> &'static str {
    match collection {
        Collection::PeerRegistry => gents_protocol::schemas::PEER_REGISTRY_NAME,
        Collection::NetworkEnrollmentRequest => {
            gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST_NAME
        }
        Collection::NetworkEnrollmentDecision => {
            gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION_NAME
        }
        Collection::NetworkAuthorizationRevision => {
            gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION_NAME
        }
        Collection::NetworkEnrollmentRouteReceipt => {
            gents_protocol::schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME
        }
        Collection::EnrollmentRenewalIntent => {
            gents_protocol::schemas::ENROLLMENT_RENEWAL_INTENT_NAME
        }
        Collection::EnrollmentRenewalOffer => {
            gents_protocol::schemas::ENROLLMENT_RENEWAL_OFFER_NAME
        }
        Collection::Other => gents_protocol::schemas::NODE_NAME,
    }
}

fn resource(resource: Resource) -> &'static str {
    match resource {
        Resource::Discovery => "discovery",
        Resource::EnrollmentRecords => "enrollment_records",
        Resource::Other => "nodes",
    }
}

#[test]
fn receiver_genesis_access_matches_the_lean_model() {
    let ids = HomePolicyIds::shared();
    let foreign = ids.id(BuiltInPolicy::Mailbox);
    family::replay::<Access, Binding, Collection, Resource>(|input| {
        let policy = match input.binding {
            Binding::HomeHousehold => ids.id(BuiltInPolicy::Household),
            Binding::OtherPolicy => foreign,
        };
        let name = collection(input.collection);
        match receiver_genesis_access(ids, name, policy, resource(input.resource)) {
            Ok(None) => Access::None,
            Ok(Some(db::GenesisReadAccess::Public)) => Access::Everyone,
            Ok(Some(db::GenesisReadAccess::AddressedBy("initial_reader_did"))) => {
                Access::AddressedReceiver
            }
            Ok(Some(db::GenesisReadAccess::AddressedBy(field))) => {
                panic!("{input:?}: addressed by {field}")
            }
            Err(GenesisReadAccessError::PolicyMismatch { collection, bound }) => {
                assert_eq!((collection.as_str(), bound.as_str()), (name, policy));
                Access::PolicyMismatch
            }
        }
    });
}
