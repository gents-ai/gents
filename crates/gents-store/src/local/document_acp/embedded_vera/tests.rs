use std::path::Path;

use gents_lean_vocab::families::document_acp_choice_reopened::{replay, Reopened};
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use serde::{Deserialize, Serialize};

use crate::local::backend_protection::BackendProtectionError;
use crate::local::document_acp::{refused, DocumentAcpError};
use crate::local::storage_backend::incompatible_store;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reopening {
    Served,
    DeploymentDiffers,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    OtherVersion,
}

fn vera_refuses_another_identity() -> defra_node::Error {
    defra_node::Error::EmbeddedVera(vera_embedded::HostError::Start(
        vera_node::EmbedError::Deployment(vera_node::SoloError::Mismatch("seeded policies")),
    ))
}

#[test]
fn reopening_matches_the_lean_model() {
    replay::<Refusal, Reopening>(|input| {
        let started = if input.same_identity {
            Ok(())
        } else {
            Err(vera_refuses_another_identity())
        };
        let Err(error) = started else {
            return Reopened {
                reopening: Reopening::Served,
                refusal: None,
            };
        };
        let error = BackendProtectionError::from(
            refused(&error).unwrap_or_else(|| panic!("{input:?}: {error}")),
        );
        let reopening = match &error {
            BackendProtectionError::DocumentAcp(DocumentAcpError::DeploymentDiffers { .. }) => {
                Reopening::DeploymentDiffers
            }
            error => panic!("{input:?}: {error}"),
        };
        let refusal = incompatible_store(&error, Path::new("data")).map(|store| match store.kind {
            IncompatibleStoreKind::OtherVersionSchema => Refusal::OtherVersion,
            kind => panic!("{input:?}: {kind:?}"),
        });
        Reopened { reopening, refusal }
    });
}
