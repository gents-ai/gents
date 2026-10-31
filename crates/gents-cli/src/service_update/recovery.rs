//! Changed builds are never restored after the candidate became launchable,
//! including exposure of its native definition before a helper crash/reboot.
//! Reconfiguration of the identical build restores settings only under the
//! existing startup and store fences, without replacing any store bytes.
use super::*;

pub(super) async fn restore_unstarted(
    plan: &Plan,
    phase: Phase,
    reservation: &mut Option<gents::home::RuntimeStartupReservation>,
) -> Result<bool> {
    let directory = operation_dir(&plan.home, plan.id);
    let opened = tokio::fs::try_exists(directory.join("startup-attempted.json")).await?
        || tokio::fs::try_exists(directory.join("candidate-exposed.json")).await?;
    // Fail closed for plans staged before the exposure marker existed, too.
    let copy = plan.clone();
    let published = tokio::task::spawn_blocking(move || -> Result<bool> {
        if opened || !copy.intent.executable_update {
            return Ok(false);
        }
        let config = service_config(&copy)?;
        let path = config.definition_path(crate::native_service::NativeServicePlatform::current()?);
        Ok(std::fs::read(path)? == copy.next_definition
            && copy.next_definition != copy.prior_definition)
    })
    .await??;
    let opened = opened || published;
    let same_build = !plan.intent.executable_update && plan.intent.digest == plan.prior_build;
    if !phase.can_restore_prior(opened, same_build) {
        return Ok(false);
    }
    if reservation.is_none() {
        if !same_build {
            return Ok(false);
        }
        *reservation = Some(
            tokio::time::timeout(
                Duration::from_secs(60),
                gents::home::reserve_runtime_startup(&plan.home),
            )
            .await
            .context("replacement startup is still active; no recovery attempted")??,
        );
    }
    if opened || phase == Phase::Retired {
        tokio::time::timeout(
            Duration::from_secs(120),
            gents::home::wait_for_store_release(
                &plan.home,
                &gents::home::default_data_dir(&plan.home),
            ),
        )
        .await
        .context("runtime still owns its store; no work was force-stopped")??;
    }
    let copy = plan.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let _store = gents::home::lock_home_store(&copy.home)?;
        let config = service_config(&copy)?;
        let manager = NativeServiceManager::new(config.clone())?;
        let installed = std::fs::read(
            config.definition_path(crate::native_service::NativeServicePlatform::current()?),
        )?;
        anyhow::ensure!(
            installed == copy.prior_definition || installed == copy.next_definition,
            "service definition was externally changed"
        );

        let prior = manager.definition_executable(&copy.prior_definition)?;
        anyhow::ensure!(
            files::digest(&prior)? == copy.prior_build,
            "prior executable changed; refusing recovery"
        );
        let init = read_init_config(&copy.home)?.context("initialized home missing")?;
        anyhow::ensure!(
            init.node_did == copy.node_did,
            "node identity changed; refusing recovery"
        );
        let network = NetworkSettings::read(&copy.home)?;
        anyhow::ensure!(
            network == copy.prior_network || network == copy.intent.network,
            "network was externally changed"
        );
        // Exclusive store ownership and the startup fence prove any native
        // respawn cannot own execution. Stop only that verified home/unit.
        if manager.status()?.is_active_or_transitioning() {
            manager.stop(false)?;
        }
        if installed == copy.next_definition && installed != copy.prior_definition {
            manager.restore_unstarted_definition(&installed, &copy.prior_definition)?;
        }
        copy.prior_network.persist(&copy.home)?;
        #[cfg(unix)]
        std::fs::File::open(&copy.home)?.sync_all()?;
        Ok(())
    })
    .await??;
    drop(reservation.take());
    let copy = plan.clone();
    tokio::task::spawn_blocking(move || {
        NativeServiceManager::new(service_config(&copy)?)?.start(false)
    })
    .await??;
    execute::verify_prior_ready(plan, Duration::from_secs(60)).await?;
    Ok(true)
}
