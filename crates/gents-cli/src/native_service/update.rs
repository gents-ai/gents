//! Native adapters for the constrained updater. No scripts or caller-selected
//! commands are accepted; argv always invokes this package's staged helper.
use super::*;
use std::collections::BTreeMap;

impl<R> NativeServiceManager<R>
where
    R: CommandRunner,
{
    pub(crate) fn update_snapshot(&self) -> Result<(Vec<u8>, PathBuf, bool)> {
        let bytes = fs::read(self.config.definition_path(self.platform))?;
        self.ensure_owned_definition(&bytes)?;
        let executable = executable(self.platform, &bytes)?;
        let status = self.status()?;
        anyhow::ensure!(
            status.running,
            "managed update requires a running native service"
        );
        Ok((bytes, executable, status.enabled))
    }

    pub(crate) fn update_definition(
        &self,
        before: &[u8],
        replacement: Option<&BTreeMap<String, String>>,
    ) -> Result<Vec<u8>> {
        self.ensure_owned_definition(before)?;
        let mut definition = String::from_utf8(before.to_vec())?;
        match self.platform {
            NativeServicePlatform::Macos => {
                let mut value = plist::Value::from_reader_xml(before)?;
                let dict = value.as_dictionary_mut().context("service dictionary")?;
                // Preserve all installed properties, including bundle association,
                // log paths and managed environment, rather than re-rendering defaults.
                let args = dict
                    .get_mut("ProgramArguments")
                    .and_then(plist::Value::as_array_mut)
                    .context("service argv")?;
                args[0] = plist::Value::String(
                    self.config
                        .executable
                        .to_str()
                        .context("executable UTF-8")?
                        .to_owned(),
                );
                if replacement.is_some() {
                    let env = dict
                        .get_mut("EnvironmentVariables")
                        .and_then(plist::Value::as_dictionary_mut)
                        .context("service environment")?;
                    env.retain(|key, _| matches!(key.as_str(), "PATH" | "GENTS_SYSTEM_LOG"));
                }
                let mut out = Vec::new();
                value.to_writer_xml(&mut out)?;
                definition = String::from_utf8(out)?;
            }
            NativeServicePlatform::Linux => {
                let old = executable(self.platform, before)?;
                let old = systemd_quote(old.to_str().context("installed executable UTF-8")?, true)?;
                let new = systemd_quote(
                    self.config
                        .executable
                        .to_str()
                        .context("executable UTF-8")?,
                    true,
                )?;
                definition = definition
                    .lines()
                    .map(|line| {
                        if line.starts_with("ExecStart=") {
                            line.replacen(&old, &new, 1)
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
            }
        }
        if let Some(replacement) = replacement {
            if self.platform == NativeServicePlatform::Linux {
                definition = environment::without_custom_directives(&definition)?;
            }
            definition = environment::apply(self.platform, definition, None, Some(replacement))?;
        }
        Ok(definition.into_bytes())
    }

    pub(crate) fn install_update_definition(
        &self,
        expected: &[u8],
        replacement: &[u8],
    ) -> Result<()> {
        let path = self.config.definition_path(self.platform);
        anyhow::ensure!(
            fs::read(&path)? == expected,
            "service definition changed since update acceptance"
        );
        self.ensure_owned_definition(replacement)?;
        anyhow::ensure!(
            !self.status()?.is_active_or_transitioning(),
            "service must be unloaded before replacement"
        );
        atomic_write(&path, replacement)?;
        if self.platform == NativeServicePlatform::Linux {
            self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
        }
        Ok(())
    }

    pub(crate) fn register_updater(&self, directory: &Path, id: uuid::Uuid) -> Result<()> {
        let label = format!("{SERVICE_LABEL}.update-{id}");
        let helper = directory.join("helper");
        let home = self.config.home.to_str().context("home UTF-8")?;
        match self.platform {
            NativeServicePlatform::Macos => {
                let mut dict = plist::Dictionary::new();
                dict.insert("Label".into(), label.clone().into());
                dict.insert(
                    "ProgramArguments".into(),
                    plist::Value::Array(
                        [
                            helper.to_str().context("helper UTF-8")?,
                            "service",
                            "update-run",
                            "--home",
                            home,
                            &id.to_string(),
                        ]
                        .into_iter()
                        .map(|s| plist::Value::String(s.to_owned()))
                        .collect(),
                    ),
                );
                dict.insert("RunAtLoad".into(), true.into());
                let mut keep = plist::Dictionary::new();
                keep.insert("SuccessfulExit".into(), false.into());
                dict.insert("KeepAlive".into(), plist::Value::Dictionary(keep));
                dict.insert("ThrottleInterval".into(), 5u64.into());
                dict.insert(
                    "StandardErrorPath".into(),
                    directory
                        .join("helper.log")
                        .to_str()
                        .context("log UTF-8")?
                        .into(),
                );
                let path = directory.join("updater.plist");
                let mut bytes = Vec::new();
                plist::Value::Dictionary(dict).to_writer_xml(&mut bytes)?;
                atomic_write(&path, &bytes)?;
                let domain = format!("gui/{}", self.current_uid()?);
                let target = format!("{domain}/{label}");
                let current =
                    self.run_allow_failure("launchctl", &["print".into(), target.clone().into()])?;
                ensure_launchd_print_result(&current)?;
                if !current.success {
                    self.run_checked(
                        "launchctl",
                        &["bootstrap".into(), domain.into(), path.into_os_string()],
                    )?;
                } else if !launchd_is_running(&current.stdout) {
                    self.run_checked("launchctl", &["kickstart".into(), target.into()])?;
                }
            }
            NativeServicePlatform::Linux => {
                let unit = format!("{label}.service");
                let quote = |s: &str| systemd_quote(s, true);
                let definition = format!("[Unit]\nDescription=Gents managed runtime update\nStartLimitIntervalSec=0\n[Service]\nType=exec\nExecStart={} service update-run --home {} {}\nRestart=on-failure\nRestartSec=5\nTimeoutStartSec=infinity\n", quote(helper.to_str().context("helper UTF-8")?)?, quote(home)?, id);
                atomic_write(
                    &self.config.service_config_dir.join(&unit),
                    definition.as_bytes(),
                )?;
                self.run_checked("systemctl", &["--user".into(), "daemon-reload".into()])?;
                self.run_checked("systemctl", &["--user".into(), "start".into(), unit.into()])?;
            }
        }
        Ok(())
    }
}

fn executable(platform: NativeServicePlatform, bytes: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(bytes)?;
    Ok(match platform {
        NativeServicePlatform::Macos => launchd_executable(text)?,
        NativeServicePlatform::Linux => systemd_executable(text)?,
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[derive(Default)]
    struct Runner {
        fail_registration: bool,
        calls: RefCell<Vec<(OsString, Vec<OsString>)>>,
    }
    impl CommandRunner for Runner {
        fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
            self.calls.borrow_mut().push((program.into(), args.into()));
            Ok(CommandOutput {
                success: !args.iter().any(|a| a == "print")
                    && !(self.fail_registration
                        && args.iter().any(|a| a == "bootstrap" || a == "start")),
                stdout: if program == OsStr::new("/usr/bin/id") {
                    "501".into()
                } else {
                    String::new()
                },
                stderr: if args.iter().any(|a| a == "print") {
                    "Could not find service".into()
                } else {
                    String::new()
                },
            })
        }
    }
    fn config(root: &Path) -> NativeServiceConfig {
        NativeServiceConfig {
            home: root.join("node"),
            executable: root.join("staged gents"),
            user_home: root.into(),
            service_config_dir: root.into(),
            search_path: Some("/bin".into()),
            associated_bundle_id: Some("ai.gents.desktop".into()),
            stderr_path: Some(root.join("stderr.log")),
        }
    }
    #[test]
    fn replacement_preserves_properties_and_environment_on_both_supervisors() {
        for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
            let dir = tempfile::tempdir().unwrap();
            let old = config(dir.path());
            let manager =
                NativeServiceManager::with_runner(old.clone(), platform, Runner::default());
            let definition = manager.render_definition().unwrap();
            let custom = BTreeMap::from([("FLEET_HOST".into(), "laptop".into())]);
            let definition = environment::apply(platform, definition, None, Some(&custom)).unwrap();
            let mut next = old;
            next.executable = dir.path().join("next binary");
            let manager =
                NativeServiceManager::with_runner(next.clone(), platform, Runner::default());
            let result = manager
                .update_definition(definition.as_bytes(), None)
                .unwrap();
            assert_eq!(executable(platform, &result).unwrap(), next.executable);
            let text = String::from_utf8(result).unwrap();
            assert!(text.contains("FLEET_HOST"));
            assert!(text.contains("laptop"));
            assert!(text.contains("stderr.log"));
            if platform == NativeServicePlatform::Macos {
                assert!(text.contains("ai.gents.desktop"));
            }
            let result = manager
                .update_definition(definition.as_bytes(), Some(&BTreeMap::new()))
                .unwrap();
            let text = String::from_utf8(result).unwrap();
            assert!(!text.contains("FLEET_HOST"));
            assert!(text.contains("GENTS_SYSTEM_LOG"));
        }
    }
    #[test]
    fn updater_is_a_separate_supervisor_job_with_fixed_argv() {
        for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
            let dir = tempfile::tempdir().unwrap();
            let manager =
                NativeServiceManager::with_runner(config(dir.path()), platform, Runner::default());
            let id = uuid::Uuid::new_v4();
            manager.register_updater(dir.path(), id).unwrap();
            let calls = manager.runner.calls.borrow();
            assert!(!calls
                .iter()
                .any(|(_, args)| args.iter().any(|arg| arg == "stop" || arg == "bootout")));
            let text = match platform {
                NativeServicePlatform::Macos => {
                    fs::read_to_string(dir.path().join("updater.plist")).unwrap()
                }
                NativeServicePlatform::Linux => fs::read_to_string(
                    dir.path()
                        .join(format!("{SERVICE_LABEL}.update-{id}.service")),
                )
                .unwrap(),
            };
            assert!(text.contains("update-run"));
            assert!(text.contains(&id.to_string()));
            assert!(!text.contains("/bin/sh"));
            assert!(!text.contains("PartOf="));
        }
    }
    #[test]
    fn registration_failure_never_stops_or_replaces_the_runtime() {
        for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
            let dir = tempfile::tempdir().unwrap();
            let runner = Runner {
                fail_registration: true,
                ..Default::default()
            };
            let config = config(dir.path());
            let path = config.definition_path(platform);
            fs::write(&path, "unchanged runtime definition").unwrap();
            let manager = NativeServiceManager::with_runner(config, platform, runner);
            assert!(manager
                .register_updater(dir.path(), uuid::Uuid::new_v4())
                .is_err());
            assert_eq!(
                fs::read_to_string(path).unwrap(),
                "unchanged runtime definition"
            );
            assert!(!manager
                .runner
                .calls
                .borrow()
                .iter()
                .any(|(_, args)| args.iter().any(|arg| arg == "stop" || arg == "bootout")));
        }
    }
}
