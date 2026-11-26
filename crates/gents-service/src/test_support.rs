use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::native_service::{
    BackgroundItemStatus, CommandOutput, CommandRunner, NativeServicePlatform, ServiceRecord,
};

#[derive(Clone, Default)]
pub struct ScriptedRunner {
    outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
    repeating: Arc<Mutex<Option<CommandOutput>>>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    pub background_item: BackgroundItemStatus,
    elapsed: Arc<Mutex<Vec<Duration>>>,
}

impl ScriptedRunner {
    pub fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
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
    pub fn then_repeat(self, output: CommandOutput) -> Self {
        *self.repeating.lock().expect("repeating mutex") = Some(output);
        self
    }

    pub fn with_background_item(mut self, status: BackgroundItemStatus) -> Self {
        self.background_item = status;
        self
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls mutex").clone()
    }

    pub fn elapsed(&self) -> Vec<Duration> {
        self.elapsed.lock().expect("elapsed mutex").clone()
    }

    pub fn total_elapsed(&self) -> Duration {
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
            Some(repeating) => Ok(CommandOutput {
                success: repeating.success,
                stdout: repeating.stdout.clone(),
                stderr: repeating.stderr.clone(),
            }),
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

#[cfg(test)]
pub(crate) use unit::{HeldManager, PreflightCandidate, Steps, StoppedSupervisor, UpdateRunner};

#[cfg(test)]
mod unit {
    use std::cell::{Cell, RefCell};
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result;

    use crate::home_state::read_init_config;
    use crate::native_service::{CommandOutput, CommandRunner, ServiceRecord, SERVICE_LABEL};
    use crate::repair::Progress;
    use gents_protocol::operations::install_repair::{Finding, RepairStep};
    use gents_store::local::store_key::{
        StoreEncryption, StoreKey, StoreKeyError, StoreKeyUnavailable,
    };

    /// Answers every supervisor query as a stopped, never-loaded job.
    #[derive(Clone, Default)]
    pub(crate) struct StoppedSupervisor {
        pub(crate) calls: Arc<Mutex<Vec<String>>>,
    }

    impl CommandRunner for StoppedSupervisor {
        fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let call = format!("{} {}", program.to_string_lossy(), args.join(" "));
            self.calls.lock().unwrap().push(call);
            let (success, stdout) = match (program.to_str().unwrap(), args.as_slice()) {
                ("/usr/bin/id", _) => (true, "501"),
                ("launchctl", [verb, ..]) if verb == "print" => (false, "Could not find service"),
                ("launchctl", [verb, ..]) if verb == "print-disabled" => (true, "{}"),
                ("systemctl", [_, verb, ..]) if verb == "is-active" => (false, "inactive"),
                ("systemctl", [_, verb, ..]) if verb == "is-enabled" => (false, "disabled"),
                _ => (true, ""),
            };
            Ok(CommandOutput {
                success,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            })
        }
    }

    #[derive(Default)]
    pub(crate) struct UpdateRunner {
        pub(crate) fail_registration: bool,
        pub(crate) calls: RefCell<Vec<(OsString, Vec<OsString>)>>,
    }

    impl CommandRunner for UpdateRunner {
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

    /// launchd and systemd as they report `record`: `launchctl print` and
    /// `systemctl --user show` render it the way the managers do.
    pub(crate) struct HeldManager {
        record: Option<ServiceRecord>,
        unloaded: Cell<bool>,
        pub(crate) calls: RefCell<Vec<Vec<String>>>,
    }

    impl HeldManager {
        pub(crate) fn new(record: Option<ServiceRecord>) -> Self {
            Self {
                record,
                unloaded: Cell::new(false),
                calls: Default::default(),
            }
        }

        fn print(&self) -> Option<String> {
            let record = self.record.as_ref().filter(|_| !self.unloaded.get())?;
            let path = record
                .definition
                .as_ref()
                .map(|path| format!("\tpath = {}\n", path.display()))
                .unwrap_or_default();
            let home = record.home.as_ref().unwrap().display();
            Some(format!(
                "gui/501/{SERVICE_LABEL} = {{\n\tactive count = 1\n{path}\ttype = LaunchAgent\n\tstate = running\n\n\tprogram = /opt/gents/gents\n\targuments = {{\n\t\t/opt/gents/gents\n\t\tserver\n\t\t--home\n\t\t{home}\n\t\t--system-log\n\t}}\n\n\tstdout path = /dev/null\n\tpid = 4242\n}}"
            ))
        }

        fn show(&self) -> String {
            match &self.record {
                None => "LoadState=not-found\nFragmentPath=\nExecStart=".to_owned(),
                Some(record) => format!(
                    "ExecStart={{ path=/usr/bin/env ; argv[]=/usr/bin/env -- /opt/gents/gents server --home {} --system-log ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}\nFragmentPath={}\nLoadState=loaded",
                    record.home.as_ref().unwrap().display(),
                    record.definition.as_ref().unwrap().display()
                ),
            }
        }

        pub(crate) fn changed(&self) -> bool {
            self.calls.borrow().iter().any(|call| {
                matches!(
                    call.get(1).map(String::as_str),
                    Some("bootout" | "bootstrap" | "kickstart" | "enable" | "disable")
                ) || call
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "stop" | "start" | "enable" | "disable"))
            })
        }
    }

    impl CommandRunner for HeldManager {
        fn run(&self, program: &OsStr, args: &[OsString]) -> anyhow::Result<CommandOutput> {
            let mut call = vec![program.to_string_lossy().into_owned()];
            call.extend(args.iter().map(|arg| arg.to_string_lossy().into_owned()));
            self.calls.borrow_mut().push(call.clone());
            let output = |success: bool, stdout: String, stderr: &str| CommandOutput {
                success,
                stdout,
                stderr: stderr.to_owned(),
            };
            Ok(match (call[0].as_str(), call.get(1).map(String::as_str)) {
                ("/usr/bin/id", _) => output(true, "501".to_owned(), ""),
                ("launchctl", Some("print")) => match self.print() {
                    Some(print) => output(true, print, ""),
                    None => output(false, String::new(), "Could not find service"),
                },
                ("launchctl", Some("bootout")) => {
                    self.unloaded.set(true);
                    output(true, String::new(), "")
                }
                ("systemctl", _) if call.iter().any(|arg| arg == "show") => {
                    output(true, self.show(), "")
                }
                _ => output(true, String::new(), ""),
            })
        }

        fn elapse(&self, delay: Duration) -> Duration {
            delay
        }
    }

    /// The candidate side of the probe: the production store-key check of the
    /// candidate's `update-check --home`, with the Keychain read injected.
    pub(crate) struct PreflightCandidate {
        pub(crate) home: PathBuf,
        pub(crate) verified: bool,
        pub(crate) readable: bool,
    }

    fn read_or_deny(
        readable: bool,
    ) -> impl FnOnce(&StoreEncryption, &Path, &Path) -> std::result::Result<StoreKey, StoreKeyError>
    {
        move |_, _, data| match readable {
            true => StoreKey::from_host(&[7; 32]),
            false => Err(StoreKeyUnavailable {
                data_path: data.into(),
                code: -25293,
            }
            .into()),
        }
    }

    impl CommandRunner for PreflightCandidate {
        fn run(&self, program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
            assert!(Path::new(program).is_file());
            assert_eq!(
                args,
                [
                    "service".into(),
                    "update-check".into(),
                    "--home".into(),
                    self.home.as_os_str().to_owned()
                ]
            );
            let config = read_init_config(&self.home)?.unwrap();
            Ok(
                match crate::service_update::preflight::check_store_key_with(
                    &self.home,
                    config.store_encryption.as_ref(),
                    read_or_deny(self.readable),
                ) {
                    Ok(()) => CommandOutput {
                        success: true,
                        stdout: crate::service_update::metadata(self.verified).to_string(),
                        stderr: String::new(),
                    },
                    Err(error) => CommandOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: format!("{:#}", anyhow::Error::from(error)),
                    },
                },
            )
        }
    }

    #[derive(Default)]
    pub(crate) struct Steps(pub(crate) Vec<RepairStep>);

    impl Progress for Steps {
        fn finding(&mut self, _: &Finding) {}
        fn step(&mut self, _: usize, _: usize, step: RepairStep) {
            self.0.push(step);
        }
    }
}
