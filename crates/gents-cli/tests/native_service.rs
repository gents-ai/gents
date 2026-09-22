use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use gents_server::native_service::{
    CommandOutput, CommandRunner, NativeServiceConfig, NativeServiceManager, NativeServicePlatform,
};

#[derive(Clone, Default)]
struct ScriptedRunner {
    outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ScriptedRunner {
    fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls mutex").clone()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
        let mut call = vec![program.to_string_lossy().into_owned()];
        call.extend(args.iter().map(|arg| arg.to_string_lossy().into_owned()));
        self.calls.lock().expect("calls mutex").push(call);
        self.outputs
            .lock()
            .expect("outputs mutex")
            .pop_front()
            .ok_or_else(|| anyhow!("unexpected native supervisor command"))
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
fn mac_loaded_but_exited_job_is_not_running() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    let runner = ScriptedRunner::with_outputs([
        output(true, "501", ""),                      // install: id
        ok(),                                         // install: disable
        output(true, "501", ""),                      // status: id
        output(true, "state = exited\npid = 44", ""), // status: print
        output(true, "501", ""),                      // status: id
        output(true, "{}", ""),                       // status: print-disabled
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
        ok(),                             // install: daemon-reload
        ok(),                             // install: disable
        output(true, "active", ""),       // restart: status is-active
        output(true, "enabled", ""),      // restart: status is-enabled
        output(false, "", "stop failed"), // restart: stop
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
        output(true, "501", ""),                     // upgrade status: id
        output(true, "state = exited", ""),          // upgrade status: print
        output(true, "501", ""),                     // upgrade status: id
        output(true, "{}", ""),                      // upgrade status: print-disabled => enabled
        output(true, "501", ""),                     // loaded inspection: id
        output(true, "state = exited", ""),          // loaded inspection: print
        output(true, "501", ""),                     // stop: id
        output(true, "state = exited", ""),          // stop: print
        ok(),                                        // stop: bootout
        output(true, "501", ""),                     // stop: wait id
        output(false, "Could not find service", ""), // stop: wait print
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
    };
    let runner = ScriptedRunner::default();
    manager(config, NativeServicePlatform::Linux, runner.clone()).stop(true)?;
    assert!(runner.calls().is_empty());
    assert!(!root.exists());
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
