use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use gents_service::native_service::{
    runtime_error_log, wait_for_process_exit, BackgroundItemStatus, CommandOutput, CommandRunner,
    NativeServiceConfig, NativeServiceManager, NativeServicePlatform, ServiceRecord, StartOutcome,
    SYSTEMD_UNIT,
};

#[cfg(target_os = "macos")]
#[path = "native_service/supervision.rs"]
mod supervision;

#[derive(Clone, Default)]
struct ScriptedRunner {
    outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
    repeating: Arc<Mutex<Option<CommandOutput>>>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    background_item: BackgroundItemStatus,
    elapsed: Arc<Mutex<Vec<Duration>>>,
}

impl ScriptedRunner {
    fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
            repeating: Arc::new(Mutex::new(None)),
            calls: Arc::new(Mutex::new(Vec::new())),
            background_item: BackgroundItemStatus::default(),
            elapsed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Answer every command after the script with `output`, so a test can pin
    /// a bound without writing out its probes.
    fn then_repeat(self, output: CommandOutput) -> Self {
        *self.repeating.lock().expect("repeating mutex") = Some(output);
        self
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls mutex").clone()
    }

    fn elapsed(&self) -> Vec<Duration> {
        self.elapsed.lock().expect("elapsed mutex").clone()
    }

    fn total_elapsed(&self) -> Duration {
        self.elapsed().into_iter().sum()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
        let mut call = vec![program.to_string_lossy().into_owned()];
        call.extend(args.iter().map(|arg| arg.to_string_lossy().into_owned()));
        self.calls.lock().expect("calls mutex").push(call);
        if let Some(output) = self.outputs.lock().expect("outputs mutex").pop_front() {
            return Ok(output);
        }
        match self.repeating.lock().expect("repeating mutex").as_ref() {
            Some(repeating) => Ok(output(
                repeating.success,
                &repeating.stdout,
                &repeating.stderr,
            )),
            None => Err(anyhow!("unexpected native supervisor command")),
        }
    }

    fn elapse(&self, delay: Duration) -> Duration {
        self.elapsed.lock().expect("elapsed mutex").push(delay);
        delay
    }

    fn background_item_status(&self, _definition: &Path) -> BackgroundItemStatus {
        self.background_item
    }

    /// The scripts hold this config's own job, if any; whose job the manager
    /// holds is replayed against the model in the repair conformance.
    fn service_record(&self, _platform: NativeServicePlatform) -> Result<Option<ServiceRecord>> {
        Ok(None)
    }
}

fn output(success: bool, stdout: &str, stderr: &str) -> CommandOutput {
    CommandOutput {
        success,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
    }
}

fn ok() -> CommandOutput {
    output(true, "", "")
}

fn config(root: &Path, name: &str) -> NativeServiceConfig {
    let home = root.join("agent-home");
    fs::create_dir_all(&home).expect("agent home");
    let executable = root.join(name);
    fs::write(&executable, b"fixture executable").expect("fixture executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("fixture executable mode");
    }
    NativeServiceConfig {
        home,
        executable,
        user_home: root.to_path_buf(),
        service_config_dir: root.join("service-definitions"),
        search_path: Some("/reviewed/bin:/usr/bin".into()),
        associated_bundle_id: None,
        stderr_path: None,
    }
}

fn manager(
    config: NativeServiceConfig,
    platform: NativeServicePlatform,
    runner: ScriptedRunner,
) -> NativeServiceManager<ScriptedRunner> {
    NativeServiceManager::with_runner(config, platform, runner)
}

#[test]
fn linux_status_preserves_runtime_enabled_service() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    for (success, unit_state, expected) in [
        (true, "enabled", true),
        (true, "enabled-runtime", true),
        (false, "disabled", false),
        (false, "masked-runtime", false),
    ] {
        let runner = ScriptedRunner::with_outputs([
            output(true, "active", ""),
            output(success, unit_state, ""),
        ]);
        let status =
            manager(config.clone(), NativeServicePlatform::Linux, runner.clone()).status()?;
        assert_eq!(status.enabled, expected, "{unit_state}");
        assert!(status.running, "{unit_state}");
        assert_eq!(
            runner.calls(),
            [
                ["systemctl", "--user", "is-active", SYSTEMD_UNIT],
                ["systemctl", "--user", "is-enabled", SYSTEMD_UNIT],
            ],
            "{unit_state}"
        );
    }
    Ok(())
}

#[test]
fn mac_restarts_clean_exits_with_bounded_throttle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let plist = plist::Value::from_file(config.definition_path(NativeServicePlatform::Macos))?;
    let definition = plist.as_dictionary().expect("service dictionary");
    assert_eq!(definition["KeepAlive"].as_boolean(), Some(true));
    assert_eq!(
        definition["ThrottleInterval"].as_unsigned_integer(),
        Some(10)
    );
    Ok(())
}

#[test]
fn linux_restarts_clean_exits_with_existing_fatal_exit_fences() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let definition = fs::read_to_string(config.definition_path(NativeServicePlatform::Linux))?;
    assert!(definition.lines().any(|line| line == "Restart=always"));
    assert!(definition.lines().any(|line| line == "RestartSec=2"));
    Ok(())
}

#[test]
fn systemd_does_not_restart_refusals() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let definition = fs::read_to_string(config.definition_path(NativeServicePlatform::Linux))?;
    assert!(definition
        .lines()
        .any(|line| line == "RestartPreventExitStatus=65 66 67 68 69 72"));
    Ok(())
}

#[test]
fn mac_loaded_but_exited_job_is_not_running() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        ok(),
        output(true, "501", ""),
        output(true, "state = exited\npid = 44", ""),
        output(true, "501", ""),
        output(true, "{}", ""),
    ]);
    let service = manager(config, NativeServicePlatform::Macos, runner);
    service.install()?;
    let status = service.status()?;
    assert!(status.installed);
    assert!(!status.running, "an exited launchd job must not be running");
    assert!(status.job_loaded, "an exited launchd job remains loaded");
    assert!(status.is_active_or_transitioning());
    assert!(status.enabled, "print-disabled did not disable the label");
    Ok(())
}

#[test]
fn mac_print_disabled_error_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        ok(),
        output(true, "501", ""),
        output(false, "Could not find service", ""),
        output(true, "501", ""),
        output(false, "", "launchctl unavailable"),
    ]);
    let service = manager(config, NativeServicePlatform::Macos, runner);
    service.install()?;
    let error = service.status().expect_err("status must fail closed");
    assert!(error
        .to_string()
        .contains("could not determine whether Gents is enabled"));
    Ok(())
}

#[test]
fn failed_stop_prevents_restart_and_preserves_definition_and_home() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    fs::write(config.home.join("durable.txt"), b"keep")?;
    let runner = ScriptedRunner::with_outputs([
        ok(),
        ok(),
        output(true, "active", ""),
        output(true, "enabled", ""),
        output(false, "", "stop failed"),
    ]);
    let service = manager(config.clone(), NativeServicePlatform::Linux, runner.clone());
    service.install()?;
    let definition = config.definition_path(NativeServicePlatform::Linux);
    let before = fs::read(&definition)?;
    assert!(service.restart().is_err(), "failed stop must fail restart");
    assert_eq!(
        fs::read(&definition)?,
        before,
        "restart must not rewrite definition after failed stop"
    );
    assert_eq!(fs::read(config.home.join("durable.txt"))?, b"keep");
    assert!(
        !runner
            .calls()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "start")),
        "restart invoked start after stop failed: {:?}",
        runner.calls()
    );
    Ok(())
}

#[test]
fn same_home_allows_a_new_caller_executable_and_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let owner = config(temp.path(), "old-gents");
    let install_runner = ScriptedRunner::with_outputs([ok(), ok()]);
    manager(owner.clone(), NativeServicePlatform::Linux, install_runner).install()?;

    let new_executable = temp.path().join("new-gents");
    fs::write(&new_executable, b"new fixture executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&new_executable, fs::Permissions::from_mode(0o755))?;
    }
    let mut new_caller =
        NativeServiceConfig::new(owner.home.join("..").join("agent-home"), new_executable)?;
    new_caller.user_home = owner.user_home.clone();
    new_caller.service_config_dir = owner.service_config_dir.clone();
    new_caller.search_path = Some("/different/caller/path".into());
    assert_eq!(
        new_caller.home,
        fs::canonicalize(&owner.home)?,
        "caller home was not canonicalized"
    );
    let runner = ScriptedRunner::with_outputs([
        output(false, "inactive", ""),
        output(false, "disabled", ""),
        ok(),
    ]);
    let service = manager(new_caller, NativeServicePlatform::Linux, runner.clone());
    let status = service.status()?;
    assert!(status.installed);
    assert!(!status.running && !status.enabled);
    service.set_enabled(true)?;
    assert!(runner
        .calls()
        .iter()
        .any(|call| call.iter().any(|arg| arg == "enable")));
    Ok(())
}

#[test]
fn foreign_home_cannot_stop_or_update_the_fixed_label_definition() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let owner = config(temp.path(), "owner-gents");
    manager(
        owner.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let definition = owner.definition_path(NativeServicePlatform::Linux);
    let before = fs::read(&definition)?;

    let mut foreign = config(temp.path(), "foreign-gents");
    foreign.home = temp.path().join("foreign-home");
    fs::create_dir_all(&foreign.home)?;
    foreign.service_config_dir = owner.service_config_dir.clone();
    let runner = ScriptedRunner::default();
    let service = manager(foreign, NativeServicePlatform::Linux, runner.clone());
    assert!(service.stop(false).is_err());
    assert!(service.install().is_err());
    assert_eq!(fs::read(&definition)?, before);
    assert!(runner.calls().is_empty(), "foreign home reached supervisor");
    Ok(())
}

#[test]
fn same_home_stopped_upgrade_unloads_old_launchd_job_without_changing_enablement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let old = config(temp.path(), "old-gents");
    let install_runner = ScriptedRunner::with_outputs([output(true, "501", ""), ok()]);
    manager(old.clone(), NativeServicePlatform::Macos, install_runner).install()?;
    let definition = old.definition_path(NativeServicePlatform::Macos);
    let before = fs::read(&definition)?;

    let mut upgraded = old.clone();
    upgraded.executable = temp.path().join("new-gents");
    fs::write(&upgraded.executable, b"new fixture executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&upgraded.executable, fs::Permissions::from_mode(0o755))?;
    }
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = exited", ""),
        output(true, "501", ""),
        output(true, "{}", ""),
        output(true, "501", ""),
        output(true, "state = exited", ""),
        output(true, "501", ""),
        output(true, "state = exited", ""),
        ok(),
        output(true, "501", ""),
        output(false, "Could not find service", ""),
    ]);
    let service = manager(upgraded, NativeServicePlatform::Macos, runner.clone());
    service.install()?;
    let after = fs::read(&definition)?;
    assert_ne!(
        after, before,
        "upgrade must replace the old executable definition"
    );
    assert!(String::from_utf8(after)?.contains("new-gents"));
    let calls = runner.calls();
    assert!(calls
        .iter()
        .any(|call| call.iter().any(|arg| arg == "bootout")));
    assert!(
        !calls
            .iter()
            .any(|call| call.iter().any(|arg| arg == "enable" || arg == "disable")),
        "stopped upgrade must preserve launchd enablement: {calls:?}"
    );
    Ok(())
}

#[test]
fn status_for_missing_definition_does_not_create_home_or_service_directories() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("missing-root");
    let config = NativeServiceConfig {
        home: root.join("agent-home"),
        executable: root.join("gents"),
        user_home: root.clone(),
        service_config_dir: root.join("service-definitions"),
        search_path: None,
        associated_bundle_id: None,
        stderr_path: None,
    };
    let status = manager(
        config,
        NativeServicePlatform::Linux,
        ScriptedRunner::default(),
    )
    .status()?;
    assert!(!status.installed && !status.running && !status.enabled);
    assert!(
        !root.exists(),
        "status must not create a host home or service directory"
    );
    Ok(())
}

#[test]
fn mac_spawn_scheduled_job_is_loaded_but_not_running() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        ok(),
        output(true, "501", ""),
        output(true, "state = spawn scheduled", ""),
        output(true, "501", ""),
        output(true, "{}", ""),
    ]);
    let service = manager(config, NativeServicePlatform::Macos, runner);
    service.install()?;
    let status = service.status()?;
    assert!(!status.running);
    assert!(status.job_loaded);
    assert!(status.is_active_or_transitioning());
    Ok(())
}

#[test]
fn stop_for_missing_definition_is_a_noop_without_host_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("missing-root");
    let config = NativeServiceConfig {
        home: root.join("agent-home"),
        executable: root.join("gents"),
        user_home: root.clone(),
        service_config_dir: root.join("service-definitions"),
        search_path: None,
        associated_bundle_id: None,
        stderr_path: None,
    };
    let runner = ScriptedRunner::default();
    manager(config, NativeServicePlatform::Linux, runner.clone()).stop(true)?;
    assert!(runner.calls().is_empty());
    assert!(!root.exists());
    Ok(())
}

/// #3737: launchd keeps and restarts a loaded job after its plist is deleted,
/// so the job is observed and stopped through launchd, not through the file.
#[test]
fn mac_loaded_job_without_a_definition_is_observed_and_stopped() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = spawn scheduled", ""),
        output(true, "501", ""),
        output(true, "state = spawn scheduled", ""),
        ok(),
        output(true, "501", ""),
        output(false, "Could not find service", ""),
    ]);
    let service = manager(
        config(temp.path(), "gents"),
        NativeServicePlatform::Macos,
        runner.clone(),
    );
    let status = service.status()?;
    assert!(!status.installed && status.job_loaded && !status.running);
    service.stop(false)?;
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "bootout"))
            .count(),
        1,
        "{calls:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn native_definition_is_private_to_the_user() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let mode = fs::metadata(config.definition_path(NativeServicePlatform::Linux))?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "native service definition must be mode 0600");
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn generated_launchd_definition_passes_plutil_lint_without_loading_it() -> Result<()> {
    use std::process::Command;

    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let plist = config.definition_path(NativeServicePlatform::Macos);
    let result = Command::new("/usr/bin/plutil")
        .args(["-lint", plist.to_str().expect("plist path utf8")])
        .output()?;
    assert!(
        result.status.success(),
        "plutil rejected generated launchd plist: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

#[path = "native_service/environment.rs"]
mod environment;

#[test]
fn a_new_definition_logs_to_the_shared_error_log() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let fixture = config(temp.path(), "gents");
    let config = NativeServiceConfig::new(fixture.home, fixture.executable)?;
    assert_eq!(
        config.stderr_path,
        Some(runtime_error_log(&config.user_home))
    );
    Ok(())
}

#[test]
fn job_pid_reads_launchd_print() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = running\n\tpid = 4312\n", ""),
    ]);
    assert_eq!(
        manager(config, NativeServicePlatform::Macos, runner).job_pid()?,
        Some(4312)
    );
    Ok(())
}

#[test]
fn linux_job_pid_zero_is_none() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([output(true, "0\n", "")]);
    assert_eq!(
        manager(config, NativeServicePlatform::Linux, runner).job_pid()?,
        None
    );
    Ok(())
}

#[test]
fn installed_home_of_a_foreign_definition_is_reported_not_refused() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut other = config(temp.path(), "gents");
    other.home = temp.path().join("other-home");
    fs::create_dir_all(&other.home)?;
    manager(
        other.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let mine = config(temp.path(), "gents");
    let manager = manager(
        mine,
        NativeServicePlatform::Macos,
        ScriptedRunner::default(),
    );
    assert_eq!(manager.installed_home()?, Some(other.home));
    Ok(())
}

#[test]
fn refreshing_keeps_the_installed_executable_and_runs_no_commands() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut old = config(temp.path(), "old-gents");
    old.stderr_path = None;
    manager(
        old.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let mut new = config(temp.path(), "new-gents");
    new.stderr_path = Some(runtime_error_log(temp.path()));
    let runner = ScriptedRunner::default();
    let manager = manager(new.clone(), NativeServicePlatform::Macos, runner.clone());
    assert!(!manager.definition_log_current()?);
    assert!(manager.refresh_definition_file()?);
    let text = fs::read_to_string(new.definition_path(NativeServicePlatform::Macos))?;
    assert!(text.contains("old-gents") && text.contains("runtime-errors.log"));
    assert!(!text.contains("new-gents"));
    assert!(runner.calls().is_empty());
    assert!(manager.definition_log_current()?);
    assert!(!manager.refresh_definition_file()?);
    Ok(())
}

#[test]
fn an_enabled_node_stays_enabled_after_a_move() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut old = config(temp.path(), "gents");
    old.home = temp.path().join("old-home");
    fs::create_dir_all(&old.home)?;
    manager(
        old.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let definition = old.definition_path(NativeServicePlatform::Macos);
    let previous = fs::read(&definition)?;
    fs::remove_file(&definition)?;
    let chosen = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        ok(),
        output(true, "501", ""),
        ok(),
    ]);
    manager(chosen.clone(), NativeServicePlatform::Macos, runner.clone())
        .install_carrying(&previous, false, true)?;
    let calls = runner.calls();
    assert_eq!(calls.last().map(|call| call[1].as_str()), Some("enable"));
    let text = fs::read_to_string(&definition)?;
    assert!(text.contains(&chosen.home.display().to_string()));
    assert!(!text.contains(&old.home.display().to_string()));
    Ok(())
}

#[test]
fn a_reinstall_keeps_the_custom_environment_and_bundle_id() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut desktop = config(temp.path(), "old-gents");
    desktop.associated_bundle_id = Some("ai.gents.desktop".into());
    manager(
        desktop.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install_with_environment(&[("CUSTOM_ENTRY".to_string(), "kept".to_string())].into())?;
    let definition = desktop.definition_path(NativeServicePlatform::Macos);
    let previous = fs::read(&definition)?;
    fs::remove_file(&definition)?;
    let mut cli = config(temp.path(), "new-gents");
    cli.associated_bundle_id = None;
    let runner = ScriptedRunner::with_outputs([output(true, "501", ""), ok()]);
    manager(cli, NativeServicePlatform::Macos, runner.clone())
        .install_carrying(&previous, true, false)?;
    let text = fs::read_to_string(&definition)?;
    assert!(text.contains("ai.gents.desktop"));
    assert!(text.contains("CUSTOM_ENTRY") && text.contains("kept"));
    assert!(text.contains("new-gents") && !text.contains("old-gents"));
    assert!(!runner.calls().iter().any(|call| call[1] == "enable"));
    Ok(())
}

#[test]
fn a_fresh_macos_install_already_logs_to_the_shared_log() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut fresh = config(temp.path(), "gents");
    fresh.stderr_path = Some(runtime_error_log(temp.path()));
    manager(
        fresh.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let path = fresh.definition_path(NativeServicePlatform::Macos);
    let before = fs::read(&path)?;
    let manager = manager(
        fresh,
        NativeServicePlatform::Macos,
        ScriptedRunner::default(),
    );
    assert!(manager.definition_log_current()?);
    assert!(!manager.refresh_definition_file()?);
    assert_eq!(fs::read(&path)?, before);
    Ok(())
}

#[test]
fn refreshing_keeps_everything_but_the_log_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut desktop = config(temp.path(), "gents");
    desktop.associated_bundle_id = Some("ai.gents.desktop".into());
    desktop.search_path = Some("/desktop/bin:/usr/bin".into());
    manager(
        desktop.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let mut cli = config(temp.path(), "gents");
    cli.associated_bundle_id = None;
    cli.search_path = None;
    cli.stderr_path = Some(runtime_error_log(temp.path()));
    assert!(
        manager(cli, NativeServicePlatform::Macos, ScriptedRunner::default())
            .refresh_definition_file()?
    );
    let text = fs::read_to_string(desktop.definition_path(NativeServicePlatform::Macos))?;
    assert!(text.contains("ai.gents.desktop"));
    assert!(text.contains("/desktop/bin:/usr/bin"));
    assert!(text.contains("runtime-errors.log"));
    Ok(())
}

#[test]
fn linux_refresh_keeps_the_installed_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut desktop = config(temp.path(), "gents");
    desktop.search_path = Some("/desktop/bin:/usr/bin".into());
    manager(
        desktop.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let mut cli = config(temp.path(), "gents");
    cli.search_path = None;
    cli.stderr_path = Some(runtime_error_log(temp.path()));
    assert!(
        manager(cli, NativeServicePlatform::Linux, ScriptedRunner::default())
            .refresh_definition_file()?
    );
    let text = fs::read_to_string(desktop.definition_path(NativeServicePlatform::Linux))?;
    assert!(text.contains("PATH=/desktop/bin:/usr/bin"));
    assert!(text.contains("runtime-errors.log"));
    Ok(())
}

#[test]
fn refreshing_a_foreign_definition_is_refused() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut other = config(temp.path(), "gents");
    other.home = temp.path().join("other-home");
    fs::create_dir_all(&other.home)?;
    manager(
        other.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let path = other.definition_path(NativeServicePlatform::Macos);
    let before = fs::read(&path)?;
    let mut mine = config(temp.path(), "gents");
    mine.stderr_path = Some(runtime_error_log(temp.path()));
    assert!(manager(
        mine,
        NativeServicePlatform::Macos,
        ScriptedRunner::default()
    )
    .refresh_definition_file()
    .is_err());
    assert_eq!(fs::read(&path)?, before);
    Ok(())
}

#[tokio::test]
async fn a_dead_pid_is_gone() {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let pid = child.id();
    child.wait().expect("wait");
    assert!(wait_for_process_exit(pid, std::time::Duration::from_secs(1)).await);
}

#[test]
fn a_start_macos_withholds_is_an_approval_wait_and_other_failures_stay_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mac = config(temp.path(), "gents");
    manager(
        mac.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let withheld = ScriptedRunner {
        background_item: BackgroundItemStatus::RequiresApproval,
        ..ScriptedRunner::default()
    };
    assert!(matches!(
        manager(mac, NativeServicePlatform::Macos, withheld.clone()).start_or_approval(false)?,
        StartOutcome::ApprovalPending { detail: Some(_) }
    ));
    assert!(withheld.calls().is_empty(), "{:?}", withheld.calls());

    let linux_root = tempfile::tempdir()?;
    let linux = config(linux_root.path(), "gents");
    manager(
        linux.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let failing = ScriptedRunner::with_outputs([
        output(false, "inactive", ""),
        output(true, "enabled", ""),
        output(false, "", "daemon-reload failed"),
    ]);
    let error = manager(linux, NativeServicePlatform::Linux, failing)
        .start_or_approval(false)
        .expect_err("a failed daemon-reload is not an approval wait");
    assert!(
        format!("{error:#}").contains("daemon-reload failed"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn mac_stop_confirms_an_unload_that_takes_longer_than_a_second() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let mut script = vec![
        output(true, "501", ""),
        output(true, "state = running", ""),
        ok(),
        output(true, "501", ""),
    ];
    script.extend((0..30).map(|_| output(true, "state = running", "")));
    script.push(output(false, "Could not find service", ""));
    let runner = ScriptedRunner::with_outputs(script);
    manager(config, NativeServicePlatform::Macos, runner.clone()).stop(false)?;
    let calls = runner.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "bootout"))
            .count(),
        1,
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|call| call[0] == "/usr/bin/id").count(),
        2,
        "the job target is resolved once per stop, not once per probe: {calls:?}"
    );
    Ok(())
}

#[test]
fn stop_waits_the_whole_supervisor_stop_budget_before_failing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = running", ""),
        ok(),
        output(true, "501", ""),
    ])
    .then_repeat(output(true, "state = running", ""));
    let error = manager(config, NativeServicePlatform::Macos, runner.clone())
        .stop(false)
        .expect_err("a job that never unloads must still fail");
    let message = format!("{error:#}");
    assert!(message.contains("did not confirm"), "{message}");
    assert!(message.contains("unloaded or became inactive"), "{message}");
    assert!(
        runner.total_elapsed() >= Duration::from_secs(120)
            && runner.total_elapsed() <= Duration::from_secs(121),
        "the wait must spend the supervisor stop budget, spent {:?}",
        runner.total_elapsed()
    );
    Ok(())
}

#[test]
fn linux_stop_confirms_a_unit_that_is_still_deactivating() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Linux,
        ScriptedRunner::with_outputs([ok(), ok()]),
    )
    .install()?;
    let mut script = vec![ok()];
    script.extend((0..25).map(|_| output(true, "deactivating", "")));
    script.push(output(true, "inactive", ""));
    script.push(ok());
    let runner = ScriptedRunner::with_outputs(script);
    manager(config, NativeServicePlatform::Linux, runner.clone()).stop(false)?;
    assert!(
        runner
            .calls()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "reset-failed")),
        "{:?}",
        runner.calls()
    );
    Ok(())
}

#[test]
fn stop_returns_on_the_first_probe_when_the_job_is_already_gone() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = running", ""),
        ok(),
        output(true, "501", ""),
        output(false, "Could not find service", ""),
    ]);
    manager(config, NativeServicePlatform::Macos, runner.clone()).stop(false)?;
    assert!(
        runner.elapsed().is_empty(),
        "a job that is already gone must not wait: {:?}",
        runner.elapsed()
    );
    assert_eq!(
        runner
            .calls()
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "print"))
            .count(),
        2,
        "{:?}",
        runner.calls()
    );
    Ok(())
}

#[test]
fn mac_bootout_already_in_progress_is_settled_by_the_wait() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = running", ""),
        output(false, "", "Boot-out failed: 36: Operation now in progress"),
        output(true, "501", ""),
        output(true, "state = running", ""),
        output(false, "Could not find service", ""),
    ]);
    manager(config.clone(), NativeServicePlatform::Macos, runner).stop(false)?;

    let refused = ScriptedRunner::with_outputs([
        output(true, "501", ""),
        output(true, "state = running", ""),
        output(false, "", "Boot-out failed: 5: Input/output error"),
    ]);
    let error = manager(config, NativeServicePlatform::Macos, refused.clone())
        .stop(false)
        .expect_err("any other boot-out refusal must still fail");
    assert!(
        format!("{error:#}").contains("launchctl failed"),
        "{error:#}"
    );
    assert!(
        !refused
            .calls()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "disable")),
        "{:?}",
        refused.calls()
    );
    Ok(())
}
