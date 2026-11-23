mod inventory;

use gents_lean_vocab::families::document_acp_choice_reopened as family;
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use serde::{Deserialize, Serialize};

use crate::local::backend_protection::BackendProtectionError;
use crate::local::document_acp::{
    ensure_present, ensure_recorded, node_dir, record, refused, Deployment, DocumentAcpError,
};
use crate::local::storage_backend::incompatible_store;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reopening {
    Served,
    DeploymentMissing,
    DeploymentDiffers,
    DeploymentReplaced,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    OtherVersion,
    Damaged,
}

fn vera_refuses_another_identity() -> defra_node::Error {
    defra_node::Error::EmbeddedVera(vera_embedded::HostError::Start(
        vera_node::EmbedError::Deployment(vera_node::SoloError::Mismatch("seeded policies")),
    ))
}

fn deployment(consensus_key: &str) -> Deployment {
    Deployment {
        consensus_key: consensus_key.into(),
        connection: Default::default(),
        transport: None,
    }
}

#[test]
fn reopening_matches_the_lean_model() {
    family::replay::<Refusal, Reopening>(|input| {
        let store = tempfile::tempdir().unwrap();
        record(store.path(), &deployment("recorded")).unwrap();
        if input.present {
            std::fs::create_dir_all(node_dir(store.path())).unwrap();
        }
        let started = deployment(if input.same_key {
            "recorded"
        } else {
            "another"
        });
        let reopened = ensure_present(store.path())
            .and_then(|()| match input.same_identity {
                true => Ok(()),
                false => Err(refused(&vera_refuses_another_identity()).expect("typed refusal")),
            })
            .and_then(|()| ensure_recorded(store.path(), Some(&started)));
        let Err(error) = reopened else {
            return family::Reopened {
                reopening: Reopening::Served,
                refusal: None,
            };
        };
        let reopening = match &error {
            DocumentAcpError::DeploymentMissing { .. } => Reopening::DeploymentMissing,
            DocumentAcpError::DeploymentDiffers { .. } => Reopening::DeploymentDiffers,
            DocumentAcpError::DeploymentReplaced { .. } => Reopening::DeploymentReplaced,
            error => panic!("{input:?}: {error}"),
        };
        let error = BackendProtectionError::from(error);
        let refusal = incompatible_store(&error, store.path()).map(|store| match store.kind {
            IncompatibleStoreKind::OtherVersionSchema => Refusal::OtherVersion,
            IncompatibleStoreKind::UnsupportedOrCorrupt => Refusal::Damaged,
            kind => panic!("{input:?}: {kind:?}"),
        });
        family::Reopened { reopening, refusal }
    });
}

#[test]
fn initial_operator_policy_matches_the_home() {
    let account =
        identity::Did::new("did:key:z6MkeXBLjYiSvqnhFb6D7sHm8yKm4jV45wwBFRaatf1cfZ76").unwrap();
    let device = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    gents_lean_vocab::families::document_acp_choice_initial_operator_threshold::replay(|input| {
        let deployment = super::deployment(device, input.shared.then_some(&account));
        let expected = if input.shared {
            vec![account.as_str(), device]
        } else {
            vec![device]
        };
        assert_eq!(deployment.operator_keys, expected);
        deployment.threshold.into()
    });
}

#[test]
fn v2_genesis_ids_match_its_seeds() {
    use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};

    let account =
        identity::Did::new("did:key:z6MkeXBLjYiSvqnhFb6D7sHm8yKm4jV45wwBFRaatf1cfZ76").unwrap();
    let device = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    let deployment = super::deployment(device, Some(&account));
    let ids = HomePolicyIds::embedded_vera_transport_v2();
    assert_eq!(deployment.policies.len(), BuiltInPolicy::ALL.len());
    for (seed, policy) in deployment.policies.iter().zip(BuiltInPolicy::ALL) {
        assert_eq!(seed.id().unwrap(), ids.id(policy), "{policy:?}");
    }
}
