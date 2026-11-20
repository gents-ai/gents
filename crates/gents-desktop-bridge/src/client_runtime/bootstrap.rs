use super::{ClientRuntime, ClientRuntimeOptions};
use anyhow::Result;
use gents_desktop_core::client::journal::ClientJournal;
use gents_desktop_core::client::journal::JournalOptions;
use gents_observe::client_projection::observe::{spawn_observer, ObservedStores};
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::client_journal::runtime_directory::RuntimeDirectory;
use std::sync::Arc;
use tokio::sync::watch;

impl ClientRuntime {
    pub async fn start() -> Result<Self> {
        Self::start_with_paths(DesktopPaths::discover()?).await
    }
    pub async fn start_with_paths(paths: DesktopPaths) -> Result<Self> {
        Self::start_with_paths_and_options(paths, ClientRuntimeOptions::default()).await
    }
    pub async fn start_with_paths_and_options(
        paths: DesktopPaths,
        options: ClientRuntimeOptions,
    ) -> Result<Self> {
        Self::start_reporting_stages(paths, options, None).await
    }
    pub async fn start_with_paths_reporting_stages(
        paths: DesktopPaths,
        options: ClientRuntimeOptions,
        completed_stage: watch::Sender<&'static str>,
    ) -> Result<Self> {
        Self::start_reporting_stages(paths, options, Some(completed_stage)).await
    }
    async fn start_reporting_stages(
        paths: DesktopPaths,
        mut options: ClientRuntimeOptions,
        completed_stage: Option<watch::Sender<&'static str>>,
    ) -> Result<Self> {
        use gents_store::identity::NodeIdentity as _;
        let journal = ClientJournal::open(
            paths.clone(),
            JournalOptions {
                store_key_custody: options.store_key_custody,
                store_key_provider: options.store_key_provider.take(),
            },
        )
        .await?;
        if let Some(stages) = &completed_stage {
            stages.send_replace("journal_ready");
        }
        let prepared = async {
            let directory = RuntimeDirectory::open(paths.runtime_directory_path()).await?;
            let stores = ObservedStores::new();
            let identity = journal.identity().clone();
            let runtime_endpoints = Arc::new(
                gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                    identity.client_signing_capability()?,
                ),
            );
            let observer =
                spawn_observer(stores.clone(), directory.clone(), runtime_endpoints.clone());
            Ok::<_, anyhow::Error>((directory, stores, identity, runtime_endpoints, observer))
        }
        .await;
        let (directory, stores, node_identity, runtime_endpoints, observer) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                journal.shutdown().await?;
                return Err(error);
            }
        };
        if let Some(stages) = completed_stage {
            stages.send_replace("runtime_observation_ready");
        }
        Ok(Self {
            owed: Default::default(),
            delivery_started: Default::default(),
            delivery_cancel: Default::default(),
            delivery_lock: Default::default(),
            access_requests: Default::default(),
            node: journal.node().clone(),
            journal,
            paths,
            options,
            node_identity,
            runtime_endpoints,
            directory,
            stores,
            observer: tokio::sync::Mutex::new(Some(observer)),
            inference_observation_epoch: Default::default(),
            inference_observations: Default::default(),
            request_patch_signatures: Default::default(),
            context_details: Default::default(),
            starters: Default::default(),
            shutdown_completion: Default::default(),
            last_mutation_error: Default::default(),
        })
    }
}

pub(super) fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "{field} is required");
    Ok(value)
}
