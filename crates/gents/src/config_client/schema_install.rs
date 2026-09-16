//! Additive schema publication shared by package installation and configuration.
//! Schema contracts are node-wide; this does not grant document read/write ACP.

use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{collection_schema_contract_digest, ConfigAccess};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaInstallPlan {
    pub artifact_digest: String,
    pub collection_contracts: BTreeMap<String, String>,
    pub requires_publication: bool,
}

/// Read-only compatibility check. Existing collections must match exactly;
/// one SDL must not mix existing and missing collections.
pub async fn preview_schema_install(access: &ConfigAccess, sdl: &str) -> Result<SchemaInstallPlan> {
    let expected = query::parse_sdl(sdl)?;
    ensure!(!expected.is_empty(), "schema declares no collection");
    let mut contracts = BTreeMap::new();
    let mut missing = false;
    let mut existing = false;
    for collection in expected {
        let digest = collection_schema_contract_digest(&serde_json::to_value(&collection)?)?;
        ensure!(
            contracts
                .insert(collection.name.clone(), digest.clone())
                .is_none(),
            "schema repeats collection {:?}",
            collection.name
        );
        match access.collection_version(&collection.name).await? {
            Some(live) => {
                existing = true;
                ensure!(
                    digest == collection_schema_contract_digest(&live)?,
                    "existing collection {:?} does not match requested schema",
                    collection.name
                );
            }
            None => missing = true,
        }
    }
    ensure!(
        !(missing && existing),
        "schema mixes existing and missing collections"
    );
    Ok(SchemaInstallPlan {
        artifact_digest: format!("sha256:{:x}", Sha256::digest(sdl.as_bytes())),
        collection_contracts: contracts,
        requires_publication: missing,
    })
}

/// Recheck contracts and the exact previewed bytes before additive publication.
/// DefraDB owns schema registration; this is not a document transaction and
/// callers must not promise atomic publication with subsequent document writes.
pub async fn apply_schema_install(
    access: &ConfigAccess,
    sdl: &str,
    expected_digest: &str,
) -> Result<SchemaInstallPlan> {
    let plan = preview_schema_install(access, sdl).await?;
    ensure!(
        plan.artifact_digest == expected_digest,
        "schema artifact changed; preview again"
    );
    if plan.requires_publication {
        access.add_schema(sdl).await?;
    }
    for (name, expected) in &plan.collection_contracts {
        let live = access
            .collection_version(name)
            .await?
            .with_context(|| format!("published collection {name:?} is not discoverable"))?;
        ensure!(
            collection_schema_contract_digest(&live)? == *expected,
            "published collection {name:?} does not match requested schema"
        );
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn additive_schema_contract_preview_digest_and_reinstall() {
        let node = std::sync::Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        let access = ConfigAccess::Local(node.clone());
        let sdl = "type EvalInput { message: String }";
        let plan = preview_schema_install(&access, sdl).await.unwrap();
        assert!(plan.requires_publication);
        assert!(node.get_collection("EvalInput").unwrap().is_none());
        assert!(apply_schema_install(&access, sdl, "wrong-digest")
            .await
            .is_err());
        assert!(node.get_collection("EvalInput").unwrap().is_none());
        apply_schema_install(&access, sdl, &plan.artifact_digest)
            .await
            .unwrap();
        assert!(
            !preview_schema_install(&access, sdl)
                .await
                .unwrap()
                .requires_publication
        );
        apply_schema_install(&access, sdl, &plan.artifact_digest)
            .await
            .unwrap();
        assert!(
            preview_schema_install(&access, "type EvalInput { message: Int }")
                .await
                .is_err()
        );
        assert!(preview_schema_install(
            &access,
            "type EvalInput { message: String } type EvalOther { message: String }"
        )
        .await
        .is_err());
        assert!(node.get_collection("EvalOther").unwrap().is_none());
    }
}
