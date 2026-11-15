use std::sync::Arc;

use identity::{Identity as _, RawIdentity};
use serde::{Deserialize, Serialize};

use super::{admit, record, Deployment, DocumentAcp, DocumentAcpError};
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Admission {
    NodeStore,
    EmbeddedVera,
    SwitchRefused,
    VeraUnavailable,
    NodeKeyCannotSign,
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
                match input.requested {
                    Provider::NodeStore => DocumentAcp::Local,
                    Provider::EmbeddedVera => DocumentAcp::EmbeddedVera,
                },
                node.as_ref(),
                input.available,
            );
            match admitted {
                Ok(DocumentAcp::Local) => Admission::NodeStore,
                Ok(DocumentAcp::EmbeddedVera) => Admission::EmbeddedVera,
                Err(DocumentAcpError::Switch { .. }) => Admission::SwitchRefused,
                Err(DocumentAcpError::Unavailable) => Admission::VeraUnavailable,
                Err(DocumentAcpError::NodeKeyCannotSign { .. }) => Admission::NodeKeyCannotSign,
                Err(error) => panic!("{input:?}: {error}"),
            }
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
    recorded["document_acp"] = "embedded-vera".into();
    let config: StoredInitConfig = serde_json::from_value(recorded.clone()).unwrap();
    assert_eq!(config.document_acp, DocumentAcp::EmbeddedVera);
    assert_eq!(serde_json::to_value(&config).unwrap(), recorded);
}
