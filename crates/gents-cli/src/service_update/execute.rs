use super::*;
use crate::commands::prepare_runtime::control;
use gents::config_client::GraphqlEndpoint;
use gents_protocol::serve_lifecycle::{
    control::{RuntimeControlDisposition, RuntimeOperation},
    ServeLifecycle,
};

pub(super) fn endpoint(raw: &str, settings: &NetworkSettings) -> Result<reqwest::Url> {
    let endpoint = reqwest::Url::parse(raw)?;
    let address = endpoint
        .host_str()
        .context("endpoint host")?
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()?;
    let expected = settings
        .http_addr
        .unwrap_or(std::net::Ipv4Addr::LOCALHOST.into());
    anyhow::ensure!(
        endpoint.scheme() == "http"
            && address == expected
            && endpoint.port_or_known_default()
                == Some(settings.http_port.unwrap_or(crate::DEFAULT_HTTP_PORT))
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.path() == "/api/v0/graphql"
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "recorded endpoint must match this home's HTTP listener"
    );
    Ok(endpoint)
}

pub(super) async fn apply(
    plan: &Plan,
    receipt: &mut Receipt,
    reservation: &mut Option<gents::home::RuntimeStartupReservation>,
) -> Result<()> {
    let directory = operation_dir(&plan.home, plan.id);
    let acceptance_deadline =
        chrono::DateTime::parse_from_rfc3339(&plan.accepted_deadline)?.with_timezone(&chrono::Utc);
    let mut changes = files::Changes::watch(&directory).await?;
    while matches!(receipt.phase, Phase::Staged | Phase::Registered) {
        anyhow::ensure!(
            chrono::Utc::now() < acceptance_deadline,
            "acceptance was not acknowledged before its deadline; runtime unchanged"
        );
        *receipt = files::read_json_async(&directory.join("receipt.json")).await?;
        if !matches!(receipt.phase, Phase::Staged | Phase::Registered) {
            break;
        }
        let remaining = (acceptance_deadline - chrono::Utc::now())
            .to_std()
            .context("acceptance deadline expired")?;
        tokio::time::timeout(remaining, changes.changed())
            .await
            .context("acceptance was not acknowledged before its deadline; runtime unchanged")??;
    }
    drop(changes);
    anyhow::ensure!(
        receipt.phase != Phase::Failed,
        "operation failed before acknowledgement"
    );
    let _keychain = crate::process_resources::prevent_keychain_prompts(true)?;
    let client = client().await?;
    // Retained maintenance state must never act on a newly initialized node.
    // This applies to every resumed phase, not only authenticated retirement.
    let verified_operator = operator(plan).await?;
    // Hold the existing startup reservation across retirement, store exclusion,
    // backup and install. A supervisor respawn cannot reopen the retiring store.
    let attempted = tokio::fs::try_exists(directory.join("startup-attempted.json")).await?;
    if attempted {
        anyhow::ensure!(
            receipt.phase == Phase::Installed,
            "startup marker conflicts with the durable update phase"
        );
    } else {
        *reservation = Some(
            tokio::time::timeout(
                Duration::from_secs(60),
                gents::home::reserve_runtime_startup(&plan.home),
            )
            .await
            .context("startup reservation is busy")??,
        );
    }
    if matches!(receipt.phase, Phase::Accepted | Phase::Retired) {
        let copy = plan.clone();
        tokio::task::spawn_blocking(move || {
            files::check_backup_capacity(&copy.home, &operation_dir(&copy.home, copy.id))
        })
        .await??;
        let url = endpoint(&plan.endpoint, &plan.prior_network)?;
        let identity = verified_operator.clone();
        let authorization = GraphqlEndpoint::as_principal(url.to_string(), identity.did());
        let deadline = deadline(&directory, "drain", plan.intent.drain_timeout_secs).await?;
        let peers_path = directory.join("expected-peers.json");
        if !tokio::fs::try_exists(&peers_path).await? {
            let peers = if plan.p2p_enabled {
                active_peers(&client, &authorization).await?
            } else {
                Vec::new()
            };
            tokio::task::spawn_blocking(move || files::write_json(&peers_path, &peers)).await??;
        }
        loop {
            anyhow::ensure!(
                chrono::Utc::now() < deadline,
                "runtime remained busy; no work was force-stopped"
            );
            let observed = control(
                &client,
                &url,
                &plan.node_did,
                RuntimeOperation::Observe,
                None,
            )
            .await?;
            let Some(observed) = observed else {
                anyhow::ensure!(
                    receipt.phase == Phase::Retired,
                    "runtime disappeared before authenticated retirement"
                );
                break;
            };
            anyhow::ensure!(
                observed.build == plan.prior_build,
                "running build changed since staging"
            );
            if observed.retiring {
                break;
            }
            let retired = control(
                &client,
                &url,
                &plan.node_did,
                RuntimeOperation::Retire {
                    expected_build: plan.prior_build.clone(),
                },
                Some(&authorization),
            )
            .await?
            .context("runtime vanished during retirement")?;
            match retired.disposition {
                RuntimeControlDisposition::Retiring | RuntimeControlDisposition::Stopping => break,
                RuntimeControlDisposition::Busy | RuntimeControlDisposition::Starting => {
                    tokio::time::sleep(Duration::from_millis(500)).await
                }
                _ => anyhow::bail!("runtime changed before authenticated retirement"),
            }
        }
        if receipt.phase == Phase::Accepted {
            receipt
                .transition(
                    Action::Retire(true),
                    "Authenticated idle retirement confirmed",
                )
                .await?;
        }
        tokio::time::timeout(
            Duration::from_secs(120),
            gents::home::wait_for_store_release(
                &plan.home,
                &gents::home::default_data_dir(&plan.home),
            ),
        )
        .await
        .context("retiring runtime did not release store")??;
        let copy = plan.clone();
        tokio::task::spawn_blocking(move || {
            NativeServiceManager::new(service_config(&copy)?)?.stop(false)
        })
        .await??;
        receipt
            .transition(
                Action::Release(true),
                "Native service unloaded and actual store lock released",
            )
            .await?;
    }
    if matches!(receipt.phase, Phase::Released | Phase::BackedUp) {
        let copy = plan.clone();
        let dir = directory.clone();
        let phase = receipt.phase;
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _store = gents::home::lock_home_store(&copy.home)?;
            let manager = NativeServiceManager::new(service_config(&copy)?)?;
            anyhow::ensure!(
                !manager.status()?.is_active_or_transitioning(),
                "native service was externally reloaded; refusing replacement"
            );
            anyhow::ensure!(
                files::digest(&service_config(&copy)?.executable)? == copy.intent.digest,
                "staged artifact changed"
            );
            if phase == Phase::Released {
                files::cold_backup(&copy, &dir)?;
            }
            Ok(())
        })
        .await??;
        if receipt.phase == Phase::Released {
            receipt
                .transition(
                    Action::Backup(true),
                    "Cold backup complete; identity and prior binary retained",
                )
                .await?;
        }
        let copy = plan.clone();
        let dir = directory.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _store = gents::home::lock_home_store(&copy.home)?;
            let network = NetworkSettings::read(&copy.home)?;
            anyhow::ensure!(
                network == copy.prior_network || network == copy.intent.network,
                "network settings changed after acceptance"
            );
            let config = service_config(&copy)?;
            let path =
                config.definition_path(crate::native_service::NativeServicePlatform::current()?);
            anyhow::ensure!(
                dir.join("backup").is_dir()
                    && std::fs::read(dir.join("backup/service-definition"))?
                        == copy.prior_definition,
                "cold backup missing or changed; refusing installation"
            );
            let manager = NativeServiceManager::new(config)?;
            let installed = std::fs::read(path)?;
            if copy.intent.executable_update {
                // A reboot or external native start can run this definition
                // after helper death. Publish the conservative migration fence
                // before making the candidate externally launchable.
                files::write_json(&dir.join("candidate-exposed.json"), &true)?;
            }
            if installed != copy.next_definition {
                manager.install_update_definition(&copy.prior_definition, &copy.next_definition)?;
            }
            copy.intent.network.persist(&copy.home)?;
            #[cfg(unix)]
            std::fs::File::open(&copy.home)?.sync_all()?;
            Ok(())
        })
        .await??;
        receipt
            .transition(
                Action::Install(true),
                "Verified executable and preserved service settings installed",
            )
            .await?;
    }
    anyhow::ensure!(
        receipt.phase == Phase::Installed,
        "unexpected resumable update phase"
    );
    let deadline = health_deadline(&directory, attempted).await?;
    if !attempted {
        let copy = plan.clone();
        tokio::task::spawn_blocking(move || {
            let target = service_config(&copy)?.executable;
            anyhow::ensure!(
                files::digest(&target)? == copy.intent.digest,
                "selected artifact changed before startup"
            );
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        let marker = directory.join("startup-attempted.json");
        tokio::task::spawn_blocking(move || files::write_json(&marker, &true)).await??;
        drop(reservation.take());
    }
    start_selected(plan).await?;
    let target_endpoint = {
        let address = plan
            .intent
            .network
            .http_addr
            .unwrap_or(std::net::Ipv4Addr::LOCALHOST.into());
        let port = plan
            .intent
            .network
            .http_port
            .unwrap_or(crate::DEFAULT_HTTP_PORT);
        let addr = std::net::SocketAddr::new(address, port);
        reqwest::Url::parse(&format!("http://{addr}/api/v0/graphql"))?
    };
    let operator = verified_operator;
    let authorization = GraphqlEndpoint::as_principal(target_endpoint.to_string(), operator.did());
    let expected_peers: Vec<String> =
        files::read_json_async(&directory.join("expected-peers.json")).await?;
    let mut peers_pending = false;
    let mut ready_seen = false;
    loop {
        if let Ok(Some(observed)) = control(
            &client,
            &target_endpoint,
            &plan.node_did,
            RuntimeOperation::Observe,
            None,
        )
        .await
        {
            anyhow::ensure!(
                observed.build == plan.intent.digest,
                "restarted service is running a different build"
            );
            if observed.lifecycle == ServeLifecycle::Ready && !observed.retiring {
                ready_seen = true;
                let connected = if plan.p2p_enabled {
                    active_peers(&client, &authorization).await
                } else {
                    Ok(Vec::new())
                };
                if peers_reconnected(&expected_peers, connected) {
                    break;
                }
                if chrono::Utc::now() >= deadline {
                    peers_pending = true;
                    break;
                }
            }
        }
        if chrono::Utc::now() >= deadline {
            anyhow::bail!(if ready_seen {
                "Updated build was ready, but latest readiness could not be confirmed. Inspect the running service before external recovery; no automatic binary rollback."
            } else {
                "Updated service readiness could not be confirmed before its deadline; it may still be starting. Inspect the running service before external recovery; cold backup retained."
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let copy = plan.clone();
    let enabled = tokio::task::spawn_blocking(move || {
        NativeServiceManager::new(service_config(&copy)?)?
            .status()
            .map(|status| status.enabled)
    })
    .await??;
    anyhow::ensure!(
        enabled == plan.enabled,
        "service login enablement changed during update; current operator setting retained"
    );
    let state: crate::shared::StoredRuntimeState =
        files::read_json_async(&plan.home.join(gents::home::RUNTIME_STATE_FILE_NAME)).await?;
    anyhow::ensure!(
        state.p2p_peer_id == plan.prior_peer_id,
        "runtime P2P identity changed during update"
    );
    let message = if peers_pending {
        "Updated build ready; previously connected peers are still pending or unobservable. No store rollback is needed.".to_owned()
    } else {
        "Updated build ready with the original node identity and expected peers reconnected; cold backup retained".into()
    };
    receipt.transition(Action::Verify(true), &message).await
}

async fn client() -> Result<reqwest::Client> {
    tokio::task::spawn_blocking(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
    })
    .await?
    .map_err(Into::into)
}
async fn operator(plan: &Plan) -> Result<std::sync::Arc<dyn gents::identity::NodeIdentity>> {
    let home = plan.home.clone();
    let identity = tokio::task::spawn_blocking(move || {
        let config = read_init_config(&home)?.context("initialized home missing")?;
        crate::home_state::load_initialized_home_identity(&home, &config)
    })
    .await??;
    anyhow::ensure!(
        identity.did() == plan.node_did.as_str(),
        "node identity changed since staging"
    );
    Ok(identity)
}
pub(super) async fn validate_live(plan: &Plan) -> Result<()> {
    let _keychain = crate::process_resources::prevent_keychain_prompts(true)?;
    let url = endpoint(&plan.endpoint, &plan.prior_network)?;
    let client = client().await?;
    let observed = control(
        &client,
        &url,
        &plan.node_did,
        RuntimeOperation::Observe,
        None,
    )
    .await?
    .context("running runtime is unavailable")?;
    anyhow::ensure!(
        observed.build == plan.prior_build
            && !observed.retiring
            && observed.lifecycle == ServeLifecycle::Ready,
        "installed executable does not match the ready running build"
    );
    require_startup_fence(&observed.capabilities)?;
    let identity = operator(plan).await?;
    // Verify operator access without requesting retirement while the caller is
    // still staging. This is the existing authenticated runtime status route.
    let authorization = GraphqlEndpoint::as_principal(url.to_string(), identity.did());
    let mut status = url;
    status.set_path("/self");
    let response = authorization.authorize(client.get(status))?.send().await?;
    anyhow::ensure!(
        response.status().is_success(),
        "runtime operator preflight was refused"
    );
    Ok(())
}
async fn active_peers(client: &reqwest::Client, endpoint: &GraphqlEndpoint) -> Result<Vec<String>> {
    let mut url = reqwest::Url::parse(endpoint.url())?;
    url.set_path("/api/v0/p2p/active-peers");
    let response = endpoint
        .authorize(client.get(url))?
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json().await?)
}

pub(super) async fn deadline(
    directory: &Path,
    name: &str,
    seconds: u64,
) -> Result<chrono::DateTime<chrono::Utc>> {
    let path = directory.join(format!("{name}-deadline.json"));
    let value: String = if tokio::fs::try_exists(&path).await? {
        files::read_json_async(&path).await?
    } else {
        let value =
            (chrono::Utc::now() + chrono::Duration::seconds(seconds.try_into()?)).to_rfc3339();
        let copy = value.clone();
        tokio::task::spawn_blocking(move || files::write_json(&path, &copy)).await??;
        value
    };
    Ok(chrono::DateTime::parse_from_rfc3339(&value)?.with_timezone(&chrono::Utc))
}

/// An unavailable peer observation is not evidence that an isolated node is
/// healthy. The native adapter must observe the peer API successfully even
/// when there were no prior connections to wait for.
pub(super) fn peers_reconnected(expected: &[String], observed: Result<Vec<String>>) -> bool {
    observed.is_ok_and(|connected| expected.iter().all(|peer| connected.contains(peer)))
}

pub(super) async fn verify_prior_ready(plan: &Plan, timeout: Duration) -> Result<()> {
    let client = client().await?;
    let endpoint = endpoint(&plan.endpoint, &plan.prior_network)?;
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(Some(observed)) = control(
                &client,
                &endpoint,
                &plan.node_did,
                RuntimeOperation::Observe,
                None,
            )
            .await
            {
                anyhow::ensure!(
                    observed.build == plan.prior_build,
                    "restored runtime build changed"
                );
                if observed.lifecycle == ServeLifecycle::Ready && !observed.retiring {
                    return Ok::<_, anyhow::Error>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context("prior runtime did not become ready after recovery")?
}

/// Replaying the startup intent must repair a kill between its durable marker
/// and the native start call, without restarting a live or migrating service.
async fn start_selected(plan: &Plan) -> Result<()> {
    let copy = plan.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let config = service_config(&copy)?;
        let definition =
            config.definition_path(crate::native_service::NativeServicePlatform::current()?);
        anyhow::ensure!(
            std::fs::read(definition)
                .context("reading installed service definition before startup")?
                == copy.next_definition,
            "installed service changed before startup"
        );
        anyhow::ensure!(
            files::digest(&config.executable)? == copy.intent.digest,
            "selected artifact changed before startup"
        );
        let manager = NativeServiceManager::new(config)?;
        if !manager.status()?.is_active_or_transitioning() {
            manager.start(false)?;
        }
        Ok(())
    })
    .await?
}

/// A resumed start gets one durable grace window, including after an offline
/// host interval. Subsequent helper crashes reuse it; drain deadlines never
/// gain a grace window and no repeated resume can extend this indefinitely.
pub(super) async fn health_deadline(
    directory: &Path,
    resumed: bool,
) -> Result<chrono::DateTime<chrono::Utc>> {
    let original = deadline(directory, "health", 120).await?;
    if resumed {
        Ok(original.max(deadline(directory, "health-resume", 120).await?))
    } else {
        Ok(original)
    }
}

pub(super) fn require_startup_fence(
    capabilities: &std::collections::BTreeMap<String, u64>,
) -> Result<()> {
    anyhow::ensure!(capabilities.get(gents_protocol::serve_lifecycle::control::NATIVE_STARTUP_FENCE_CAPABILITY) == Some(&1), "running runtime lacks the native startup fence required for safe managed updates; use external administration for its first upgrade");
    Ok(())
}
