//! Additive schema publication shared by package installation and configuration.
//! Schema contracts are node-wide; this does not grant document read/write ACP.

use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::schema_contract::{collection_schema_field_delta, SchemaFieldDelta};
use super::{collection_schema_contract_digest, ConfigAccess};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaInstallPlan {
    pub artifact_digest: String,
    pub collection_contracts: BTreeMap<String, String>,
    pub requires_publication: bool,
    pub field_deltas: BTreeMap<String, SchemaFieldDelta>,
}

impl SchemaInstallPlan {
    /// An SDL-plus-patch series may retain extra fields, but must supply every
    /// declared field before reporting successful convergence.
    pub fn require_satisfied(&self) -> Result<()> {
        ensure!(
            !self.requires_publication,
            "declared schema collections are not published"
        );
        for (collection, delta) in &self.field_deltas {
            ensure!(
                delta.pending.is_empty(),
                "collection {collection:?} is missing declared fields {:?}",
                delta.pending
            );
        }
        Ok(())
    }
}

/// Read-only compatibility check. Existing collections must match exactly;
/// one SDL must not mix existing and missing collections.
pub async fn preview_schema_install(access: &ConfigAccess, sdl: &str) -> Result<SchemaInstallPlan> {
    preview_schema(access, sdl, false).await
}

/// Plan an SDL phase of an additive patch series. The caller must finish its
/// patches and re-preview with `require_satisfied` before claiming convergence.
pub async fn preview_additive_schema_install(
    access: &ConfigAccess,
    sdl: &str,
) -> Result<SchemaInstallPlan> {
    preview_schema(access, sdl, true).await
}

async fn preview_schema(
    access: &ConfigAccess,
    sdl: &str,
    additive: bool,
) -> Result<SchemaInstallPlan> {
    let expected = query::parse_sdl(sdl)?;
    ensure!(!expected.is_empty(), "schema declares no collection");
    let mut contracts = BTreeMap::new();
    let mut missing = false;
    let mut existing = false;
    let mut field_deltas = BTreeMap::new();
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
                let delta =
                    collection_schema_field_delta(&serde_json::to_value(&collection)?, &live)
                        .with_context(|| {
                            format!(
                                "existing collection {:?} does not match requested schema",
                                collection.name
                            )
                        })?;
                ensure!(
                    additive || (delta.pending.is_empty() && delta.extra.is_empty()),
                    "existing collection {:?} does not match requested schema",
                    collection.name
                );
                field_deltas.insert(collection.name.clone(), delta);
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
        field_deltas,
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
    apply_schema(access, sdl, expected_digest, false).await
}

pub async fn apply_additive_schema_install(
    access: &ConfigAccess,
    sdl: &str,
    expected_digest: &str,
) -> Result<SchemaInstallPlan> {
    apply_schema(access, sdl, expected_digest, true).await
}

async fn apply_schema(
    access: &ConfigAccess,
    sdl: &str,
    expected_digest: &str,
    additive: bool,
) -> Result<SchemaInstallPlan> {
    let plan = preview_schema(access, sdl, additive).await?;
    ensure!(
        plan.artifact_digest == expected_digest,
        "schema artifact changed; preview again"
    );
    if plan.requires_publication {
        access.add_schema(sdl).await?;
    }
    let observed = preview_schema(access, sdl, additive).await?;
    ensure!(
        !observed.requires_publication,
        "published schema is not discoverable"
    );
    if plan.requires_publication {
        observed.require_satisfied()?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn additive_series_preserves_exact_install_and_requires_declared_fields() {
        let node = std::sync::Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        let access = ConfigAccess::Local(node.clone());
        let base = "type EvalPatch { message: String }";
        let extended = "type EvalPatch { message: String owned_files: String @immutable }";
        let plan = preview_schema_install(&access, base).await.unwrap();
        assert!(plan.require_satisfied().is_err());
        apply_schema_install(&access, base, &plan.artifact_digest)
            .await
            .unwrap();
        let pending = preview_additive_schema_install(&access, extended)
            .await
            .unwrap();
        assert_eq!(
            pending.field_deltas["EvalPatch"]
                .pending
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["owned_files"]
        );
        assert!(pending.require_satisfied().is_err());
        assert!(preview_schema_install(&access, extended).await.is_err());
        apply_additive_schema_install(&access, extended, &pending.artifact_digest)
            .await
            .unwrap();
        assert!(preview_additive_schema_install(&access, extended)
            .await
            .unwrap()
            .require_satisfied()
            .is_err());
        node.patch_collection("EvalPatch", r#"[{"op":"add","path":"/EvalPatch/Fields/-","value":{"Name":"owned_files","Kind":"String","Immutable":true}}]"#).await.unwrap();
        preview_additive_schema_install(&access, extended)
            .await
            .unwrap()
            .require_satisfied()
            .unwrap();
        let repeated = preview_additive_schema_install(&access, base)
            .await
            .unwrap();
        assert!(repeated.field_deltas["EvalPatch"]
            .extra
            .contains("owned_files"));
        repeated.require_satisfied().unwrap();
        assert!(preview_schema_install(&access, base).await.is_err());
        for conflicting in [
            "type EvalPatch { message: Int }",
            "type EvalPatch { message: String owned_files: String }",
        ] {
            assert!(
                preview_additive_schema_install(&access, conflicting)
                    .await
                    .is_err(),
                "{conflicting}"
            );
        }
        node.shutdown().await;
    }

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
