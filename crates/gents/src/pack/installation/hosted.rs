use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::documents::{
    document_graph_stages, ensure_plugin_references_resolve, hosted_document_graph_from_stages,
    stage_document_plugins, HostedDocumentGraph, HostedPackPreparation,
    PreparedDocumentPackInstall,
};
use super::{DriftPolicy, InstallReport};
use crate::pack::archive::PackArchive;
use crate::store::config_client::{ConfigAccess, PackApprovalScope, PackEffectApprover};

pub struct HostedHomePublication {
    pub home: std::path::PathBuf,
    pub record: crate::pack::HomePackInstall,
    pub leases: Vec<crate::store::local::file_lock::FileLock>,
}

async fn publish_hosted_home(publication: HostedHomePublication) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let _leases = publication.leases;
        crate::pack::record_after_install(
            &publication.home,
            publication.record,
            crate::pack::InstalledVia::Operator,
            true,
        )
    })
    .await??;
    Ok(())
}

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
    archive: Arc<PackArchive>,
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
    let manifest = archive.manifest().clone();
    let graph_archive = archive.clone();
    let graph_prepared = prepared.clone();
    let (stages, graph) = tokio::task::spawn_blocking(move || {
        let stages = document_graph_stages(&graph_archive, &graph_prepared, explicit)?;
        let graph = Arc::new(hosted_document_graph_from_stages(&stages, &graph_prepared)?);
        Ok::<_, anyhow::Error>((stages, graph))
    })
    .await??;
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
        home_publication: Option<HostedHomePublication>,
    ) -> std::result::Result<Vec<InstallReport>, HostedPackStageError> {
        let access = access.clone();
        let owner = owner.to_owned();
        tokio::spawn(async move {
            self.apply_owned(&access, &owner, scope, approver, policy, home_publication)
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
        home_publication: Option<HostedHomePublication>,
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
        let previous = self
            .stage
            .as_ref()
            .map(|stage| stage.previous_plugins())
            .unwrap_or_default();
        let installed = self.installed_plugins().to_vec();
        let finish = approver.clone();
        let result = access
            .transact_verified_pack_with_approver(
                "pack.document_graph.install",
                scope,
                approver,
                |txn| {
                    let graph = graph.clone();
                    let finish = finish.clone();
                    let previous = previous.clone();
                    let installed = installed.clone();
                    Box::pin(async move {
                        txn.interpreter()
                            .stage_plugin_delete_roots(
                                &previous
                                    .iter()
                                    .map(|plugin| plugin.projected())
                                    .collect::<Vec<_>>(),
                                &installed
                                    .iter()
                                    .map(|plugin| plugin.projected())
                                    .collect::<Vec<_>>(),
                            )
                            .await?;
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
        let home_result = if result.is_ok() {
            if let Some(publication) = home_publication {
                publish_hosted_home(publication)
                    .await
                    .context("pack documents committed but home install record failed")
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
        if let Some(stage) = self.stage.take() {
            stage
                .finish(!rejected)
                .await
                .context("finishing hosted plugin stage")
                .map_err(HostedPackStageError::Unconfirmed)?;
        }
        home_result.map_err(HostedPackStageError::Unconfirmed)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hosted_home_publication_conforms_to_lean() {
        let cases = tokio::task::spawn_blocking(|| {
            crate::lean_vocab_test::lean_contract_snapshot()
                .hosted_pack_home_cases
                .clone()
        })
        .await
        .unwrap();
        for case in cases {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path().join("node");
            let writable = case["home_written"].as_bool().unwrap();
            if !writable {
                let path = home.clone();
                tokio::task::spawn_blocking(move || std::fs::write(path, "not a directory"))
                    .await
                    .unwrap()
                    .unwrap();
            }
            let publication = HostedHomePublication {
                home: home.clone(),
                record: crate::pack::HomePackInstall {
                    coordinate: "fixture/hosted".into(),
                    version: "1".into(),
                    digest: "sha256:fixture".into(),
                    kind: crate::pack::PackKind::Documents,
                    assets: String::new(),
                    dependency_assets: Vec::new(),
                    plugins: Vec::new(),
                    installed_at: "2026-01-01T00:00:00Z".into(),
                    ux: Vec::new(),
                    installed_via: Some(crate::pack::InstalledVia::Operator),
                },
                leases: Vec::new(),
            };
            let result = publish_hosted_home(publication).await;
            assert_eq!(
                result.is_ok(),
                case["reported_success"].as_bool().unwrap(),
                "{case}: {result:?}"
            );
            if writable {
                assert!(crate::pack::read_home_install(&home, "fixture/hosted")
                    .unwrap()
                    .is_some());
            }
        }
    }
}
