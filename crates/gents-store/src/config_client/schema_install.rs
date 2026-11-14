use std::collections::BTreeMap;

type Result<T, E = SchemaInstallError> = std::result::Result<T, E>;
use gents_protocol::operations::schema::{LiveCollection, SchemaInstallDecision};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::schema_contract::{
    collection_schema_field_delta, SchemaContractError, SchemaFieldDelta,
};
use super::{collection_schema_contract_digest, ConfigAccess};

#[derive(Debug, thiserror::Error)]
#[error("existing collection {collection:?} does not match requested schema")]
pub struct SchemaInstallMismatch {
    pub collection: String,
    #[source]
    pub source: Option<SchemaContractError>,
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaInstallPlanError {
    #[error("declared schema collections are not published")]
    NotPublished,
    #[error("collection {collection:?} is missing declared fields {pending}")]
    MissingFields { collection: String, pending: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaInstallError {
    #[error(transparent)]
    Parse(#[from] query::QueryError),
    #[error(transparent)]
    Contract(#[from] SchemaContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Mismatch(#[from] SchemaInstallMismatch),
    #[error(transparent)]
    Plan(#[from] SchemaInstallPlanError),
    #[error("schema declares no collection")]
    NoCollections,
    #[error("schema repeats collection {collection:?}")]
    RepeatedCollection { collection: String },
    #[error("schema mixes existing collections {existing:?} and missing collections {missing:?}. Preview/install the missing collections separately; do not repeat existing definitions in that installation")]
    MixedCollections {
        existing: Vec<String>,
        missing: Vec<String>,
    },
    #[error("collection {collection:?} exists without declared fields {pending:?}; add them with a collection update, or apply this schema together with the patches that add them")]
    FieldsPending {
        collection: String,
        pending: Vec<String>,
    },
    #[error("schema artifact changed; preview again")]
    ArtifactChanged,
    #[error("published schema is not discoverable")]
    NotDiscoverable,
}

impl SchemaInstallError {
    pub fn mismatch(&self) -> Option<&SchemaInstallMismatch> {
        match self {
            Self::Mismatch(mismatch) => Some(mismatch),
            _ => None,
        }
    }

    /// Lean `SchemaInstall.Decision`: which refusal this failure is, when it
    /// is one.
    pub fn refusal(&self) -> Option<SchemaInstallDecision> {
        match self {
            Self::Mismatch(_) => Some(SchemaInstallDecision::Mismatch),
            Self::FieldsPending { .. } => Some(SchemaInstallDecision::FieldsPending),
            _ => None,
        }
    }
}

/// Lean `SchemaInstall.decide`.
pub fn schema_install_decision(live: LiveCollection, patches: bool) -> SchemaInstallDecision {
    match live {
        LiveCollection::Missing => SchemaInstallDecision::Publish,
        LiveCollection::Exact | LiveCollection::Extra => SchemaInstallDecision::Satisfied,
        LiveCollection::Pending if patches => SchemaInstallDecision::AwaitsFields,
        LiveCollection::Pending => SchemaInstallDecision::FieldsPending,
        LiveCollection::Conflicting => SchemaInstallDecision::Mismatch,
    }
}

/// Lean `SchemaInstall.requestAdmitted`.
pub fn schema_request_admitted(any_missing: bool, any_live: bool) -> bool {
    !(any_missing && any_live)
}

/// How a declared collection is compared with a live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comparison {
    /// A signed artifact verified against the node: any field difference is a
    /// mismatch.
    Exact,
    /// A client verb, decided by [`schema_install_decision`].
    Client { patches: bool },
}

impl Comparison {
    fn admit(
        self,
        collection: &str,
        delta: std::result::Result<&SchemaFieldDelta, SchemaContractError>,
    ) -> Result<()> {
        let mismatch = |source| SchemaInstallMismatch {
            collection: collection.to_owned(),
            source,
        };
        match self {
            Self::Exact => {
                let delta = delta.map_err(|source| mismatch(Some(source)))?;
                if !delta.pending.is_empty() || !delta.extra.is_empty() {
                    return Err(mismatch(None).into());
                }
                Ok(())
            }
            Self::Client { patches } => {
                let live = match &delta {
                    Err(_) => LiveCollection::Conflicting,
                    Ok(delta) if !delta.pending.is_empty() => LiveCollection::Pending,
                    Ok(delta) if !delta.extra.is_empty() => LiveCollection::Extra,
                    Ok(_) => LiveCollection::Exact,
                };
                match schema_install_decision(live, patches) {
                    SchemaInstallDecision::Mismatch => Err(mismatch(delta.err()).into()),
                    SchemaInstallDecision::FieldsPending => {
                        Err(SchemaInstallError::FieldsPending {
                            collection: collection.to_owned(),
                            pending: delta
                                .map(|delta| delta.pending.iter().cloned().collect())
                                .unwrap_or_default(),
                        })
                    }
                    SchemaInstallDecision::Publish
                    | SchemaInstallDecision::Satisfied
                    | SchemaInstallDecision::AwaitsFields => Ok(()),
                }
            }
        }
    }
}

fn install_error_in<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a SchemaInstallError> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause.downcast_ref::<SchemaInstallError>()
    })
}

/// The install mismatch anywhere in an application error's chain, including
/// through [`SchemaInstallError`], which wraps it transparently.
pub fn schema_install_mismatch<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a SchemaInstallMismatch> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause.downcast_ref::<SchemaInstallMismatch>()
    })
    .or_else(|| install_error_in(error)?.mismatch())
}

/// Which install refusal an application error carries anywhere in its chain.
pub fn schema_install_refusal(
    error: &(dyn std::error::Error + 'static),
) -> Option<SchemaInstallDecision> {
    schema_install_mismatch(error)
        .map(|_| SchemaInstallDecision::Mismatch)
        .or_else(|| install_error_in(error)?.refusal())
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
    preview_schema(access, sdl, Comparison::Exact).await
}

/// The schema install of a client verb. `patches` says the same request
/// carries the patches that add declared fields a live collection lacks.
pub async fn preview_client_schema_install(
    access: &ConfigAccess,
    sdl: &str,
    patches: bool,
) -> Result<SchemaInstallPlan> {
    preview_schema(access, sdl, Comparison::Client { patches }).await
}

async fn preview_schema(
    access: &ConfigAccess,
    sdl: &str,
    comparison: Comparison,
) -> Result<SchemaInstallPlan> {
    let expected = query::parse_sdl(sdl)?;
    if expected.is_empty() {
        return Err(SchemaInstallError::NoCollections);
    }
    let mut contracts = BTreeMap::new();
    let mut missing = Vec::new();
    let mut existing = Vec::new();
    let mut field_deltas = BTreeMap::new();
    for collection in expected {
        let digest = collection_schema_contract_digest(&serde_json::to_value(&collection)?)?;
        if contracts
            .insert(collection.name.clone(), digest.clone())
            .is_some()
        {
            return Err(SchemaInstallError::RepeatedCollection {
                collection: collection.name.clone(),
            });
        }
        match access.collection_version(&collection.name).await? {
            Some(live) => {
                existing.push(collection.name.clone());
                match collection_schema_field_delta(&serde_json::to_value(&collection)?, &live) {
                    Ok(delta) => {
                        comparison.admit(&collection.name, Ok(&delta))?;
                        field_deltas.insert(collection.name.clone(), delta);
                    }
                    Err(source) => comparison.admit(&collection.name, Err(source))?,
                }
            }
            None => missing.push(collection.name),
        }
    }
    if !schema_request_admitted(!missing.is_empty(), !existing.is_empty()) {
        return Err(SchemaInstallError::MixedCollections { existing, missing });
    }
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
    apply_schema(access, sdl, expected_digest, Comparison::Exact).await
}

pub async fn apply_client_schema_install(
    access: &ConfigAccess,
    sdl: &str,
    expected_digest: &str,
    patches: bool,
) -> Result<SchemaInstallPlan> {
    apply_schema(access, sdl, expected_digest, Comparison::Client { patches }).await
}

async fn apply_schema(
    access: &ConfigAccess,
    sdl: &str,
    expected_digest: &str,
    comparison: Comparison,
) -> Result<SchemaInstallPlan> {
    let plan = preview_schema(access, sdl, comparison).await?;
    if plan.artifact_digest != expected_digest {
        return Err(SchemaInstallError::ArtifactChanged);
    }
    if plan.requires_publication {
        access.add_collection(sdl).await?;
    }
    let observed = preview_schema(access, sdl, comparison).await?;
    if observed.requires_publication {
        return Err(SchemaInstallError::NotDiscoverable);
    }
    if plan.requires_publication {
        observed.require_satisfied()?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_decisions_match_lean() {
        use gents_lean_vocab::families;
        families::schema_install_decide::replay(|i| schema_install_decision(i.live, i.patches));
        families::schema_install_request_admitted::replay(|i| {
            schema_request_admitted(i.any_missing, i.any_live)
        });
    }

    #[test]
    fn install_mismatch_classifies_through_its_owner() {
        let mismatch = || SchemaInstallMismatch {
            collection: "EvalPatch".into(),
            source: None,
        };
        for error in [
            anyhow::Error::new(mismatch()),
            SchemaInstallError::from(mismatch()).into(),
        ] {
            let error = error.context("schema install");
            assert_eq!(
                schema_install_mismatch(error.as_ref()).map(|found| found.collection.as_str()),
                Some("EvalPatch")
            );
        }
        let other: anyhow::Error = SchemaInstallError::NoCollections.into();
        assert!(schema_install_mismatch(other.as_ref()).is_none());
    }

    #[tokio::test]
    async fn additive_series_preserves_exact_install_and_requires_declared_fields() {
        let node = std::sync::Arc::new(defra_node::Node::builder().build().await.unwrap());
        let access = ConfigAccess::local(node.clone());
        let base = "type EvalPatch { message: String }";
        let extended = "type EvalPatch { message: String owned_files: String @immutable }";
        let plan = preview_schema_install(&access, base).await.unwrap();
        assert!(plan.require_satisfied().is_err());
        apply_schema_install(&access, base, &plan.artifact_digest)
            .await
            .unwrap();
        let pending = preview_client_schema_install(&access, extended, true)
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
        apply_client_schema_install(&access, extended, &pending.artifact_digest, true)
            .await
            .unwrap();
        assert!(preview_client_schema_install(&access, extended, true)
            .await
            .unwrap()
            .require_satisfied()
            .is_err());
        access
            .patch_collection_schema(
                "EvalPatch",
                &serde_json::json!([{"op":"add","path":"/EvalPatch/Fields/-","value":{"Name":"owned_files","Kind":"String","Immutable":true}}]),
            )
            .await
            .unwrap();
        preview_client_schema_install(&access, extended, true)
            .await
            .unwrap()
            .require_satisfied()
            .unwrap();
        let repeated = preview_client_schema_install(&access, base, true)
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
                preview_client_schema_install(&access, conflicting, true)
                    .await
                    .is_err(),
                "{conflicting}"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }

    #[tokio::test]
    async fn additive_schema_contract_preview_digest_and_reinstall() {
        let node = std::sync::Arc::new(defra_node::Node::builder().build().await.unwrap());
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
