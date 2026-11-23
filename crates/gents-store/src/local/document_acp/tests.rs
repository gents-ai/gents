use std::sync::Arc;

use identity::{Identity as _, RawIdentity};
use serde::{Deserialize, Serialize};

use super::{admit, enter, record, Deployment, DocumentAcp, DocumentAcpError};
use crate::identity::{KeyIdentity, NodeIdentity, RegisteredIdentity};
use crate::local::home::StoredInitConfig;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    Fresh,
    CreatedOnNodeStore,
    CreatedOnEmbeddedVera,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Provider {
    NodeStore,
    EmbeddedVera,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Admission {
    NodeStore,
    EmbeddedVera,
    SwitchRefused,
    VeraUnavailable,
    NodeKeyCannotSign,
}

fn document_acp(provider: Provider) -> DocumentAcp {
    match provider {
        Provider::NodeStore => DocumentAcp::Local,
        Provider::EmbeddedVera => DocumentAcp::Vera,
    }
}

fn enclave_like_identity() -> Arc<dyn NodeIdentity> {
    let raw = RawIdentity::from_secp256r1(crypto::generate_secp256r1().unwrap()).unwrap();
    let did = raw.did().unwrap().to_string();
    defra_core::signing::store_identity(
        &did,
        defra_core::signing::SigningConfig {
            key_type: defra_core::signing::SigningKeyType::Secp256r1,
            private_key_bytes: defra_core::signing::SigningConfig::private_key_bytes_from_vec(
                raw.private_key_bytes(),
            ),
            public_key_bytes: raw.public_key_bytes(),
            public_key_hex: String::new(),
            remote_signer: None,
            signing_authorization: None,
        },
    )
    .unwrap();
    Arc::new(RegisteredIdentity::from_registered_did(&did, None).unwrap())
}

#[test]
fn admission_matches_the_lean_model() {
    let keys = tempfile::tempdir().unwrap();
    let signing: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(keys.path().join("node.key"), None).unwrap());
    let enclave_like = enclave_like_identity();
    gents_lean_vocab::families::document_acp_choice_admit::replay::<Admission, Provider, Record>(
        |input| {
            let store = tempfile::tempdir().unwrap();
            if matches!(input.store, Record::CreatedOnEmbeddedVera) {
                record(
                    store.path(),
                    &Deployment {
                        consensus_key: "recorded".into(),
                    },
                )
                .unwrap();
            }
            let node = if input.node_key_signs {
                &signing
            } else {
                &enclave_like
            };
            let admitted = admit(
                store.path(),
                matches!(input.store, Record::Fresh),
                document_acp(input.requested),
                node.as_ref(),
                input.available,
            );
            let entered = enter(
                match input.store {
                    Record::Fresh => None,
                    Record::CreatedOnNodeStore => Some(DocumentAcp::Local),
                    Record::CreatedOnEmbeddedVera => Some(DocumentAcp::Vera),
                },
                document_acp(input.requested),
                input.available,
                input.node_key_signs,
            );
            let [admitted, entered] = [admitted, entered].map(|decided| match decided {
                Ok(DocumentAcp::Local) => Admission::NodeStore,
                Ok(DocumentAcp::Vera) => Admission::EmbeddedVera,
                Err(DocumentAcpError::Switch { .. }) => Admission::SwitchRefused,
                Err(DocumentAcpError::Unavailable) => Admission::VeraUnavailable,
                Err(DocumentAcpError::NodeKeyCannotSign { .. }) => Admission::NodeKeyCannotSign,
                Err(error) => panic!("{input:?}: {error}"),
            });
            assert_eq!(
                admitted, entered,
                "{input:?}: the init config's choice is entered as its store admits it"
            );
            admitted
        },
    );
}

#[test]
fn the_choice_round_trips_through_the_init_config() {
    let unrecorded = serde_json::json!({
        "home": "/home",
        "node_name": "node",
        "node_did": "did:key:node",
        "key_path": null,
        "tool_ceiling": "meta-only",
        "tool_root": null,
    });
    let config: StoredInitConfig = serde_json::from_value(unrecorded.clone()).unwrap();
    assert_eq!(config.document_acp, DocumentAcp::Local);
    assert_eq!(serde_json::to_value(&config).unwrap(), unrecorded);

    let mut recorded = unrecorded;
    recorded["document_acp"] = "vera".into();
    let config: StoredInitConfig = serde_json::from_value(recorded.clone()).unwrap();
    assert_eq!(config.document_acp, DocumentAcp::Vera);
    assert_eq!(serde_json::to_value(&config).unwrap(), recorded);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reported {
    Absent,
    Named,
    Unavailable,
}

#[test]
fn the_reported_delegation_audience_matches_the_lean_model() {
    use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
    gents_lean_vocab::families::identity_user_delegation_status_reports::replay::<Provider, Reported>(
        |input| {
            let audience = input.node_reports_audience.then(|| DelegationAudience {
                worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
                deployment: 7,
                genesis_id: None,
            });
            match super::status_reports(document_acp(input.provider), audience.clone()) {
                ReportedDelegationAudience::Absent => Reported::Absent,
                ReportedDelegationAudience::Named(named) => {
                    assert_eq!(Some(named), audience);
                    Reported::Named
                }
                ReportedDelegationAudience::Unavailable => Reported::Unavailable,
            }
        },
    );
}

#[test]
fn signing_with_a_held_user_key_matches_the_lean_model() {
    gents_lean_vocab::families::identity_user_delegation_runtime_signs_with_held_user_key::replay::<
        Provider,
    >(
        |input| match super::runtime_signs_with_held_user_key(document_acp(input.provider)) {
            Ok(()) => true,
            Err(DocumentAcpError::HeldUserKey) => false,
            Err(error) => panic!("{error}"),
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Author {
    User,
    Node,
    Agent,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Presented {
    IdentityToken,
    Delegation,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Edge {
    Unchanged,
    DelegationRequired,
}

#[test]
fn the_http_edge_matches_the_lean_model() {
    use crate::config_client::user_write::Author as Caller;
    gents_lean_vocab::families::identity_user_delegation_edge::replay::<
        Author,
        Edge,
        Presented,
        Provider,
    >(|input| {
        let caller = match input.author {
            Author::User => Caller::User,
            Author::Node => Caller::Node,
            Author::Agent => Caller::Agent,
        };
        let presented = match input.presented {
            Presented::IdentityToken => super::Presented::IdentityToken,
            Presented::Delegation => super::Presented::Delegation,
        };
        match super::edge(document_acp(input.provider), caller, presented) {
            Ok(()) => Edge::Unchanged,
            Err(DocumentAcpError::DelegationRequired) => Edge::DelegationRequired,
            Err(error) => panic!("{error}"),
        }
    });
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Publication {
    Proceeds,
    AwaitsTransportGrants,
}

#[test]
fn node_publication_matches_the_lean_model() {
    gents_lean_vocab::families::identity_node_publication_publication::replay::<
        Provider,
        Publication,
    >(|input| {
        match super::publication_with_admission(document_acp(input.provider), input.admitted) {
            super::Publication::Proceeds => Publication::Proceeds,
            super::Publication::AwaitsTransportGrants => Publication::AwaitsTransportGrants,
        }
    });
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Route {
    Direct,
    OwnerStaged,
}

#[test]
fn configuration_grant_route_matches_the_lean_model() {
    gents_lean_vocab::families::identity_owner_staged_configuration_grant_route::replay::<
        Provider,
        Route,
    >(|input| {
        match super::configuration_grant_route(document_acp(input.provider), input.actor_owns) {
            super::ConfigurationGrantRoute::Direct => Route::Direct,
            super::ConfigurationGrantRoute::OwnerStaged => Route::OwnerStaged,
        }
    });
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Periodic {
    RegistryHeartbeat,
    EndpointHeartbeat,
    Directory,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Loop {
    Beats,
    Idle,
}

#[test]
fn access_wakes_match_the_lean_model() {
    gents_lean_vocab::families::access_wake_listens::replay::<Provider>(|input| {
        super::access_wakes(document_acp(input.provider))
    });
}

#[test]
fn periodic_publication_matches_the_lean_model() {
    gents_lean_vocab::families::identity_node_publication_periodic::replay::<
        Loop,
        Periodic,
        Provider,
    >(|input| {
        let periodic = match input.periodic {
            Periodic::RegistryHeartbeat => super::Periodic::RegistryHeartbeat,
            Periodic::EndpointHeartbeat => super::Periodic::EndpointHeartbeat,
            Periodic::Directory => super::Periodic::Directory,
        };
        match super::periodic(document_acp(input.provider), periodic) {
            super::PeriodicLoop::Beats => Loop::Beats,
            super::PeriodicLoop::Idle => Loop::Idle,
        }
    });
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Root {
    Created,
    Absent,
}

#[test]
fn enrollment_root_matches_the_lean_model() {
    gents_lean_vocab::families::identity_node_publication_enrollment_root::replay::<Provider, Root>(
        |input| match super::enrollment_root(document_acp(input.provider)) {
            super::EnrollmentRoot::Created => Root::Created,
            super::EnrollmentRoot::Absent => Root::Absent,
        },
    );
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EnrollmentRequest {
    Offer,
    Decision,
    Pending,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Answer {
    Served,
    PeerNetworkingUnavailable,
}

#[test]
fn enrollment_matches_the_lean_model() {
    gents_lean_vocab::families::identity_node_publication_enrollment::replay::<
        Answer,
        EnrollmentRequest,
        Provider,
    >(|input| {
        let request = match input.enrollment_request {
            EnrollmentRequest::Offer => super::EnrollmentRequest::Offer,
            EnrollmentRequest::Decision => super::EnrollmentRequest::Decision,
            EnrollmentRequest::Pending => super::EnrollmentRequest::Pending,
        };
        match super::enrollment(document_acp(input.provider), request) {
            super::EnrollmentAnswer::Served => Answer::Served,
            super::EnrollmentAnswer::PeerNetworkingUnavailable => Answer::PeerNetworkingUnavailable,
        }
    });
}

#[test]
fn what_a_person_reads_names_no_internals() {
    let path = std::path::PathBuf::from("/home/data/vera/node");
    let refusals = [
        DocumentAcpError::Switch {
            recorded: DocumentAcp::Local,
            requested: DocumentAcp::Vera,
        },
        DocumentAcpError::Unavailable,
        DocumentAcpError::NodeKeyCannotSign {
            node: "did:key:z6Mk".into(),
        },
        DocumentAcpError::HeldUserKey,
        DocumentAcpError::DelegationRequired,
        DocumentAcpError::TransportGrantsAwaitProvider,
        DocumentAcpError::DeploymentDiffers {
            what: "consensus key",
        },
        DocumentAcpError::DeploymentMissing { path: path.clone() },
        DocumentAcpError::DeploymentReplaced { path },
    ];
    let delegation = [crate::config_client::DelegationError::AudienceUnavailable];
    let read = refusals
        .iter()
        .map(ToString::to_string)
        .chain(delegation.iter().map(ToString::to_string))
        .chain([
            crate::config_client::user_write::REFUSED_FOR_A_PERSON.to_owned(),
            crate::config_client::owed::OWED_FAILED_FOR_A_PERSON.to_owned(),
        ]);
    for text in read {
        let lowered = text.replace("/home/data/vera/node", "").to_lowercase();
        let words: Vec<&str> = lowered
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();
        for internal in [
            "vera",
            "defradb",
            "acp",
            "delegation",
            "audience",
            "registration",
            "deployment",
            "credential",
            "chain",
        ] {
            assert!(!words.contains(&internal), "{text:?} names {internal:?}");
        }
        assert!(!lowered.contains("access control"), "{text:?}");
    }
}

#[test]
fn policy_ids_match_the_lean_model() {
    gents_lean_vocab::families::document_acp_choice_policy_ids::replay::<super::PolicyIds, Provider>(
        |input| super::policy_ids(document_acp(input.provider)),
    );
}
