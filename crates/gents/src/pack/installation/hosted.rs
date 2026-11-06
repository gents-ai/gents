use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::documents::{
    document_graph_stages, ensure_plugin_references_resolve, hosted_document_graph,
    stage_document_plugins, HostedDocumentGraph, HostedPackPreparation,
    PreparedDocumentPackInstall,
};
use super::{DriftPolicy, InstallReport};
use crate::pack::archive::PackArchive;
use crate::store::config_client::{ConfigAccess, PackApprovalScope, PackEffectApprover};

/// Holds the runtime's plugin-store lease through the native User transaction.
pub struct HostedPackStage {
    stage: Option<crate::plugin::install::StagedPluginInstall>,
    graph: Arc<HostedDocumentGraph>,
    preparation: HostedPackPreparation,
    node_did: String,
    pack_digests: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HostedPackStageError {
    #[error(transparent)]
    Rejected(anyhow::Error),
    #[error(transparent)]
    Unconfirmed(anyhow::Error),
}

pub async fn stage_hosted_pack(
    owner: &str,
    home: &Path,
    archive: &PackArchive,
    prepared: Arc<PreparedDocumentPackInstall>,
    consent: bool,
    explicit: bool,
) -> Result<HostedPackStage> {
    let pack_digests = std::iter::once(archive.digest().to_owned())
        .chain(
            prepared
                .dependencies
                .iter()
                .map(|dependency| dependency.digest.clone()),
        )
        .collect();
    let stages = document_graph_stages(archive, &prepared, explicit)?;
    let graph = Arc::new(hosted_document_graph(archive, &prepared, explicit)?);
    let stage = stage_document_plugins(&stages, Some(home), consent, owner).await?;
    let installed = stage
        .as_ref()
        .map(|stage| stage.installed.clone())
        .unwrap_or_default();
    let references = stages
        .iter()
        .map(|stage| (stage.manifest.clone(), stage.config.clone()))
        .collect::<Vec<_>>();
    let digests: BTreeMap<_, _> = installed
        .iter()
        .map(|plugin| {
            (
                format!("{}/{}", plugin.namespace, plugin.name),
                plugin.digest.clone(),
            )
        })
        .collect();
    let home = home.to_owned();
    let manifest = archive.manifest().clone();
    let preparation = tokio::task::spawn_blocking(move || {
        let refs = references
            .iter()
            .map(|(manifest, config)| (manifest, config))
            .collect::<Vec<_>>();
        ensure_plugin_references_resolve(&refs, Some(&home), &digests)?;
        prepared.hosted_preparation(&manifest, &home)
    })
    .await??;
    Ok(HostedPackStage {
        stage,
        graph,
        preparation,
        node_did: owner.to_owned(),
        pack_digests,
    })
}

impl HostedPackStage {
    pub fn preparation(&self) -> &HostedPackPreparation {
        &self.preparation
    }

    pub fn installed_plugins(&self) -> &[crate::plugin::store::InstalledPlugin] {
        self.stage
            .as_ref()
            .map(|stage| stage.installed.as_slice())
            .unwrap_or(&[])
    }

    pub async fn apply(
        self,
        access: &ConfigAccess,
        owner: &str,
        scope: PackApprovalScope,
        approver: Arc<dyn PackEffectApprover>,
        policy: DriftPolicy,
    ) -> std::result::Result<Vec<InstallReport>, HostedPackStageError> {
        let access = access.clone();
        let owner = owner.to_owned();
        tokio::spawn(async move {
            self.apply_owned(&access, &owner, scope, approver, policy)
                .await
        })
        .await
        .map_err(|error| {
            HostedPackStageError::Unconfirmed(
                anyhow::Error::new(error).context("owned hosted pack installation task failed"),
            )
        })?
    }

    async fn apply_owned(
        mut self,
        access: &ConfigAccess,
        owner: &str,
        scope: PackApprovalScope,
        approver: Arc<dyn PackEffectApprover>,
        policy: DriftPolicy,
    ) -> std::result::Result<Vec<InstallReport>, HostedPackStageError> {
        if !(owner == self.node_did
            && scope.node_did == self.node_did
            && scope.pack_digests == self.pack_digests
            && !scope.nonce.is_empty())
        {
            return Err(HostedPackStageError::Rejected(anyhow::anyhow!(
                "hosted pack approval scope does not match the staged archive graph"
            )));
        }
        let graph = self.graph.clone();
        let finish = approver.clone();
        let result = access
            .transact_verified_pack_with_approver(
                "pack.document_graph.install",
                scope,
                approver,
                |txn| {
                    let graph = graph.clone();
                    let finish = finish.clone();
                    Box::pin(async move {
                        let reports = super::documents::apply_hosted_document_graph(
                            txn.interpreter(),
                            owner,
                            &graph,
                            policy,
                        )
                        .await?;
                        finish.client_finished(txn.interpreter()).await?;
                        Ok::<_, anyhow::Error>(reports)
                    })
                },
            )
            .await;
        let rejected = result
            .as_ref()
            .err()
            .is_some_and(|error| !error.unconfirmed);
        if let Some(stage) = self.stage.take() {
            stage
                .finish(!rejected)
                .await
                .context("finishing hosted plugin stage")
                .map_err(HostedPackStageError::Unconfirmed)?;
        }
        result.map_err(|error| {
            if rejected {
                HostedPackStageError::Rejected(error.error.context(
                    "pack document transaction was rejected; local plugin records were restored",
                ))
            } else {
                HostedPackStageError::Unconfirmed(error.error.context(
                    "pack document outcome was not confirmed; local plugins remain published",
                ))
            }
        })
    }
}
