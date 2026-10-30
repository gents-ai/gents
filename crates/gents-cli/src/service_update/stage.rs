use super::files::{
    backup_inventory, copy_executable, digest, private_dir, read_json, validate_native_executable,
    write_json,
};
use super::*;
use std::fs::{self, File};

pub(super) fn prepare(
    home: PathBuf,
    id: uuid::Uuid,
    args: ServiceUpdateArgs,
) -> Result<(Plan, Receipt, File)> {
    let root = home.join(gents::home::SERVICE_UPDATES_DIR_NAME);
    private_dir(&root)?;
    let submission_lock = super::files::submission_lock(&root.join("submission.lock"))?;
    let directory = operation_dir(&home, id);
    if directory.join("plan.json").is_file() {
        let plan: Plan = read_json(&directory.join("plan.json"))?;
        let environment = args
            .environment_file
            .as_deref()
            .map(crate::native_service::read_environment_file)
            .transpose()?;
        let digest = if args.executable.is_some() {
            args.sha256
                .as_deref()
                .context("--executable requires --sha256")?
                .to_ascii_lowercase()
        } else {
            plan.prior_build.clone()
        };
        let intent = Intent {
            digest,
            executable_update: args.executable.is_some(),
            network: plan.prior_network.apply(&args.network),
            environment,
            drain_timeout_secs: args.drain_timeout_secs,
        };
        anyhow::ensure!(
            plan.intent == intent,
            "operation ID already belongs to a different update"
        );
        return Ok((
            plan,
            read_json(&directory.join("receipt.json"))?,
            submission_lock,
        ));
    }
    let current = std::env::current_exe()?;
    let config = NativeServiceConfig::new(home.clone(), current.clone())?;
    let manager = NativeServiceManager::new(config.clone())?;
    let (definition, old_executable, enabled) = manager.update_snapshot()?;
    backup_inventory(&home)?;
    let prior_network = NetworkSettings::read(&home)?;
    let network = prior_network.apply(&args.network);
    network.validate()?;
    let environment = args
        .environment_file
        .as_deref()
        .map(crate::native_service::read_environment_file)
        .transpose()?;
    let candidate = args.executable.as_deref().unwrap_or(&old_executable);
    let expected = digest(candidate)?;
    if args.executable.is_some() {
        let supplied = args
            .sha256
            .as_deref()
            .context("--executable requires --sha256 from a trusted source")?;
        anyhow::ensure!(
            supplied.eq_ignore_ascii_case(&expected),
            "candidate SHA-256 mismatch; runtime unchanged"
        );
    }
    super::files::check_backup_capacity(&home, &root)?;
    let intent = Intent {
        digest: expected.clone(),
        executable_update: args.executable.is_some(),
        network,
        environment,
        drain_timeout_secs: args.drain_timeout_secs,
    };
    reject_pending(&root)?;
    let init = read_init_config(&home)?.context("home is not initialized")?;
    let state = read_runtime_state(&home)?.context("runtime has no endpoint")?;
    anyhow::ensure!(
        init.node_did == state.node_did
            && fs::canonicalize(&state.home)? == home
            && fs::canonicalize(&init.home)? == home,
        "runtime and initialized home disagree"
    );
    // No hostname resolution or redirects: the signed control response binds
    // identity/build; the persisted listener binds the endpoint to this home.
    execute::endpoint(&state.graphql, &prior_network)?;
    let temp = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(&root)?;
    private_dir(temp.path())?;
    copy_executable(candidate, &temp.path().join("gents"))?;
    anyhow::ensure!(
        digest(&temp.path().join("gents"))? == expected,
        "artifact changed during staging"
    );
    copy_executable(&current, &temp.path().join("helper"))?;
    copy_executable(&old_executable, &temp.path().join("prior-gents"))?;
    validate_native_executable(&temp.path().join("gents"))?;
    // The digest is checked before executing the candidate. Candidate-owned
    // preflight inspects format markers without opening or modifying the store.
    use crate::native_service::{CommandRunner as _, ProcessCommandRunner};
    let helper_probe = ProcessCommandRunner.run(
        temp.path().join("helper").as_os_str(),
        &["service".into(), "update-check".into()],
    )?;
    anyhow::ensure!(
        helper_probe.success,
        "staged updater cannot execute; runtime unchanged: {}",
        gents::failure_text::bounded_text(&helper_probe.stderr)
    );
    let helper_metadata: serde_json::Value = serde_json::from_str(&helper_probe.stdout)?;
    anyhow::ensure!(
        helper_metadata["handoff_version"] == 1,
        "staged updater protocol changed; runtime unchanged"
    );
    let probe = ProcessCommandRunner.run(
        temp.path().join("gents").as_os_str(),
        &[
            "service".into(),
            "update-check".into(),
            "--home".into(),
            home.as_os_str().to_owned(),
        ],
    )?;
    anyhow::ensure!(
        probe.success,
        "candidate preflight refused; runtime unchanged: {}",
        gents::failure_text::bounded_text(&probe.stderr)
    );
    let probe: serde_json::Value = serde_json::from_str(&probe.stdout)?;
    anyhow::ensure!(
        probe["handoff_version"] == 1
            && probe["schema_checked"] == true
            && probe["os"] == std::env::consts::OS
            && probe["arch"] == std::env::consts::ARCH,
        "candidate platform or handoff protocol mismatch"
    );
    let target = if intent.executable_update {
        directory.join("gents")
    } else {
        old_executable.clone()
    };
    let config = NativeServiceConfig::new(home.clone(), target.clone())?;
    let next_definition = NativeServiceManager::new(config.clone())?
        .update_definition(&definition, intent.environment.as_ref())?;
    anyhow::ensure!(
        NativeServiceManager::new(config.clone())?.definition_executable(&next_definition)?
            == target,
        "rendered service executable does not match the verified target"
    );
    // The asynchronous owner verifies that the installed executable's digest
    // equals the signed live build before registering any updater.
    let plan = Plan {
        version: 1,
        service_config_dir: config.service_config_dir.clone(),
        user_home: config.user_home.clone(),
        id,
        home,
        node_did: init.node_did,
        session_id: std::env::var("GENTS_CALLER_SESSION_ID").ok(),
        request_id: std::env::var("GENTS_CALLER_REQUEST_ID").ok(),
        endpoint: state.graphql,
        prior_build: digest(&old_executable)?,
        intent,
        prior_network,
        prior_definition: definition,
        next_definition,
        enabled,
        p2p_enabled: state.p2p_transport != "none",
        prior_peer_id: state.p2p_peer_id,
        accepted_deadline: (chrono::Utc::now() + chrono::Duration::seconds(300)).to_rfc3339(),
    };
    let receipt = Receipt {
        operation_id: id,
        node_did: plan.node_did.clone(),
        session_id: plan.session_id.clone(),
        request_id: plan.request_id.clone(),
        phase: Phase::Staged,
        message: "Verified candidate staged; runtime unchanged".into(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        directory: directory.clone(),
    };
    write_json(&temp.path().join("plan.json"), &plan)?;
    write_json(&temp.path().join("receipt.json"), &receipt)?;
    fs::rename(temp.keep(), &directory)?;
    File::open(&root)?.sync_all()?;
    Ok((plan, receipt, submission_lock))
}

/// Unpublished staging directories cannot own an operation. An abandoned
/// pre-acceptance operation expires under the same CAS lock as acknowledgement;
/// an accepted operation is never cleared merely because its helper is absent.
pub(super) fn reject_pending(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
        else {
            continue;
        };
        let directory = entry.path();
        let path = directory.join("receipt.json");
        if !path.is_file() {
            continue;
        }
        let mut receipt: Receipt = read_json(&path)?;
        anyhow::ensure!(receipt.operation_id == id, "operation receipt ID mismatch");
        if matches!(receipt.phase, Phase::Staged | Phase::Registered) {
            let plan: Plan = read_json(&directory.join("plan.json"))?;
            if chrono::Utc::now() >= chrono::DateTime::parse_from_rfc3339(&plan.accepted_deadline)?
            {
                // The CAS can lose to acknowledgement. Re-read the durable
                // state; an accepted winner must continue blocking new work.
                let _ = receipt.transition_sync(
                    Action::Fail,
                    "Acceptance deadline expired before retirement; runtime unchanged",
                );
                receipt = read_json(&path)?;
            }
        }
        anyhow::ensure!(matches!(receipt.phase, Phase::Healthy | Phase::Failed), "update {} already pending; inspect service update-status before submitting another operation", receipt.operation_id);
    }
    Ok(())
}
