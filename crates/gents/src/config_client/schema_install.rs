use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::schema_contract::{collection_schema_field_delta, SchemaFieldDelta};
use super::{collection_schema_contract_digest, ConfigAccess};

#[derive(Debug, thiserror::Error)]
#[error("existing collection {collection:?} does not match requested schema")]
pub(crate) struct SchemaInstallMismatch {
    pub collection: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaInstallPlanError {
    #[error("declared schema collections are not published")]
    NotPublished,
    #[error("collection {collection:?} is missing declared fields {pending}")]
    MissingFields { collection: String, pending: String },
}

/// Schema publication is node-wide and does not grant document read/write ACP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaInstallPlan {
    pub artifact_digest: String,
    pub collection_contracts: BTreeMap<String, String>,
    pub requires_publication: bool,
    pub field_deltas: BTreeMap<String, SchemaFieldDelta>,
}

impl SchemaInstallPlan {
    pub fn require_satisfied(&self) -> Result<(), SchemaInstallPlanError> {
        if self.requires_publication {
            return Err(SchemaInstallPlanError::NotPublished);
        }
        for (collection, delta) in &self.field_deltas {
            if !delta.pending.is_empty() {
                return Err(SchemaInstallPlanError::MissingFields {
                    collection: collection.clone(),
                    pending: format!("{:?}", delta.pending),
                });
            }
        }
        Ok(())
    }
}

pub async fn preview_schema_install(access: &ConfigAccess, sdl: &str) -> Result<SchemaInstallPlan> {
    preview_schema(access, sdl, false).await
}

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
    let mut missing = Vec::new();
    let mut existing = Vec::new();
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
                existing.push(collection.name.clone());
                let delta =
                    collection_schema_field_delta(&serde_json::to_value(&collection)?, &live)
                        .with_context(|| SchemaInstallMismatch {
                            collection: collection.name.clone(),
                        })?;
                if !additive && (!delta.pending.is_empty() || !delta.extra.is_empty()) {
                    return Err(SchemaInstallMismatch {
                        collection: collection.name,
                    }
                    .into());
                }
                field_deltas.insert(collection.name.clone(), delta);
            }
            None => missing.push(collection.name),
        }
    }
    ensure!(
        missing.is_empty() || existing.is_empty(),
        "schema mixes existing collections {existing:?} and missing collections {missing:?}. Preview/install the missing collections separately; do not repeat existing definitions in that installation"
    );
    Ok(SchemaInstallPlan {
        artifact_digest: format!("sha256:{:x}", Sha256::digest(sdl.as_bytes())),
        collection_contracts: contracts,
        requires_publication: !missing.is_empty(),
        field_deltas,
    })
}

/// Recheck contracts and the exact previewed bytes before additive publication;
/// this is not a document transaction and must not be promised atomic with
/// subsequent document writes.
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
        let access = ConfigAccess::local(node.clone());
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
        let access = ConfigAccess::local(node.clone());
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
