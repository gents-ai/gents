use super::{home, planning, service};
use crate::native_service::{
    CommandOutput, CommandRunner, NativeServiceConfig, NativeServiceManager, NativeServicePlatform,
    ServiceHolder, ServiceRecord, SERVICE_LABEL, SYSTEMD_UNIT,
};
use gents_lean_vocab::families;
use gents_protocol::operations::install_repair::{HomeState, RepairObservation};
use serde_json::json;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[test]
fn install_repair_home_classification_matches_lean() {
    for case in families::install_repair_classification::cases::<String, String>() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let actual = home::classify_observation(&input);
        case.check(serde_json::from_value(serde_json::to_value(actual).unwrap()).unwrap());
    }
}

#[test]
fn install_repair_plans_match_lean() {
    for case in
        families::install_repair_plans::cases::<String, String, String, String, String, String>()
    {
        let input: RepairObservation =
            serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let actual = planning::plan(&input);
        case.check(serde_json::from_value(serde_json::to_value(actual).unwrap()).unwrap());
    }
}

#[test]
fn install_repair_refusals_match_lean() {
    for case in families::install_repair_refusals::cases::<String>() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let actual = planning::refusal_action(&input);
        case.check(serde_json::from_value(serde_json::to_value(actual).unwrap()).unwrap());
    }
}

/// A machine on which the service manager holds the job a Lean
/// `ManagerRecord` describes, laid out on disk: the chosen home, the
/// definition path under the asking `HOME`, and what the manager records.
struct HeldMachine {
    _root: tempfile::TempDir,
    chosen: PathBuf,
    expected: PathBuf,
    record: Option<ServiceRecord>,
}

impl HeldMachine {
    fn new(record: &serde_json::Value, platform: NativeServicePlatform) -> Self {
        let root = tempfile::tempdir().unwrap();
        let chosen = root.path().join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        let file = match platform {
            NativeServicePlatform::Macos => format!("{SERVICE_LABEL}.plist"),
            NativeServicePlatform::Linux => SYSTEMD_UNIT.to_owned(),
        };
        let expected = root.path().join("asking").join("services").join(&file);
        let bit = |field: &str| record[field].as_bool().unwrap();
        let record = match record["recorded"].as_str().unwrap() {
            "none" => None,
            recorded => {
                let definition = if bit("at_expected_path") {
                    expected.clone()
                } else {
                    root.path().join("user").join("services").join(&file)
                };
                if bit("definition_present") {
                    std::fs::create_dir_all(definition.parent().unwrap()).unwrap();
                    std::fs::write(&definition, "a definition the manager loaded").unwrap();
                }
                let home = match recorded {
                    "chosen" => chosen.clone(),
                    "other" => {
                        let other = root.path().join("served");
                        if bit("home_present") {
                            std::fs::create_dir_all(&other).unwrap();
                        }
                        other
                    }
                    other => panic!("unknown recorded home {other}"),
                };
                Some(ServiceRecord {
                    definition: Some(definition),
                    home: Some(home),
                })
            }
        };
        Self {
            _root: root,
            chosen,
            expected,
            record,
        }
    }

    fn holder(&self) -> ServiceHolder {
        ServiceHolder::of(self.record.as_ref(), &self.chosen, &self.expected)
    }

    fn manager(&self, platform: NativeServicePlatform) -> NativeServiceManager<HeldManager> {
        let config = NativeServiceConfig {
            home: self.chosen.clone(),
            executable: PathBuf::from("/opt/gents/gents"),
            user_home: self._root.path().join("asking"),
            service_config_dir: self.expected.parent().unwrap().to_path_buf(),
            search_path: None,
            associated_bundle_id: None,
            stderr_path: None,
        };
        NativeServiceManager::with_runner(config, platform, HeldManager::new(self.record.clone()))
    }
}

/// launchd and systemd as they report `record`: `launchctl print` and
/// `systemctl --user show` render it the way the managers do.
struct HeldManager {
    record: Option<ServiceRecord>,
    unloaded: std::cell::Cell<bool>,
    calls: std::cell::RefCell<Vec<Vec<String>>>,
}

impl HeldManager {
    fn new(record: Option<ServiceRecord>) -> Self {
        Self {
            record,
            unloaded: std::cell::Cell::new(false),
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
            "gui/501/{SERVICE_LABEL} = {{\n\tactive count = 1\n{path}\ttype = LaunchAgent\n\tstate = running\n\n\tprogram = /opt/gents/gents\n\targuments = {{\n\t\t/opt/gents/gents\n\t\tserver\n\t\t--home\n\t\t{home}\n\t}}\n\n\tstdout path = /dev/null\n\tpid = 4242\n}}"
        ))
    }

    fn show(&self) -> String {
        match &self.record {
            None => "LoadState=not-found\nFragmentPath=\nExecStart=".to_owned(),
            Some(record) => format!(
                "ExecStart={{ path=/usr/bin/env ; argv[]=/usr/bin/env -- GENTS_SYSTEM_LOG=1 /opt/gents/gents server --home {} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}\nFragmentPath={}\nLoadState=loaded",
                record.home.as_ref().unwrap().display(),
                record.definition.as_ref().unwrap().display()
            ),
        }
    }

    fn changed(&self) -> bool {
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

    fn elapse(&self, delay: std::time::Duration) -> std::time::Duration {
        delay
    }
}

/// Whose job the manager holds is read from what launchd and systemd report
/// of it, and a stop through this home's manager changes the label only when
/// that job is not another home's.
#[test]
fn install_repair_service_holders_match_lean() {
    for case in families::install_repair_service_holders::cases::<String, String>() {
        let record = serde_json::to_value(&case.input).unwrap();
        for platform in [NativeServicePlatform::Macos, NativeServicePlatform::Linux] {
            let machine = HeldMachine::new(&record, platform);
            let manager = machine.manager(platform);
            let observed = manager.holder().unwrap();
            assert_eq!(observed, machine.holder(), "{record} {platform:?}");
            case.check(holder_name(&observed).to_owned());
            if platform == NativeServicePlatform::Macos && !machine.expected.is_file() {
                let stopped = manager.stop(false);
                assert_eq!(
                    stopped.is_ok(),
                    observed.may_change(),
                    "{record} {stopped:?}"
                );
                assert_eq!(
                    manager.runner().changed(),
                    observed.may_change() && machine.record.is_some(),
                    "{record} {:?}",
                    manager.runner().calls.borrow()
                );
            }
        }
    }
}

fn holder_name(holder: &ServiceHolder) -> &'static str {
    match holder {
        ServiceHolder::Free => "free",
        ServiceHolder::Chosen => "chosen",
        ServiceHolder::Stale => "stale",
        ServiceHolder::Orphan => "orphan",
        ServiceHolder::Another { .. } => "another",
    }
}

/// The holder of a case's record on a machine laid out for it.
fn held(record: &serde_json::Value) -> ServiceHolder {
    if record["recorded"] == "none" {
        return ServiceHolder::Free;
    }
    HeldMachine::new(record, NativeServicePlatform::Macos).holder()
}

#[test]
fn install_repair_service_plans_match_lean() {
    use super::port::{PortObservation, ServedIdentity};
    use gents_protocol::operations::install_repair::{RepairOperation, ServiceMove};
    use service::{NodeVersion, ServiceFacts};
    for case in
        families::install_repair_service_plans::cases::<String, String, String, String, String>()
    {
        let input = &case.input;
        let record = serde_json::to_value(input).unwrap()["record"].clone();
        let home = PathBuf::from("/chosen");
        let facts = ServiceFacts {
            definition_home: input.definition_present.then(|| {
                if input.same_home {
                    home.clone()
                } else {
                    PathBuf::from("/other")
                }
            }),
            executable_present: input.executable_present,
            log_current: input.log_current,
            job_loaded: input.loaded,
            job_running: input.running,
            job_failed: input.failed,
            was_serving: input.was_serving,
            home: if input.home_complete {
                HomeState::Healthy
            } else {
                HomeState::MissingSettings
            },
            service_move: if input.before_setup {
                ServiceMove::BeforeSetup
            } else {
                ServiceMove::AfterSetup
            },
            node_version: match input.version.as_str() {
                "unknown" => None,
                "older" => Some(NodeVersion::Older(None)),
                "same" => Some(NodeVersion::Same),
                "newer" => Some(NodeVersion::Newer(semver::Version::new(999, 0, 0))),
                other => panic!("unknown version relation {other}"),
            },
            foreign_program: input.foreign_program,
            managed_update: input.managed_update,
            drain_supported: false,
            previous_version: None,
            port: match input.serving.as_str() {
                "none" => PortObservation::NotListening,
                "same" => PortObservation::Gents {
                    identity: ServedIdentity::Matches,
                    payload: json!({}),
                },
                "different" => PortObservation::Gents {
                    identity: ServedIdentity::Different {
                        node_did: None,
                        home: None,
                    },
                    payload: json!({}),
                },
                other => panic!("unknown serving identity {other}"),
            },
            served_home_present: input.served_home_present,
            operation: serde_json::from_value::<RepairOperation>(json!(input.operation)).unwrap(),
            holder: held(&record),
        };
        let actual = wire(service::plan_service(&facts, &home));
        case.check(serde_json::from_value(json!(actual)).unwrap());
    }
}

pub(super) struct Steps(pub(super) Vec<gents_protocol::operations::install_repair::RepairStep>);

impl super::Progress for Steps {
    fn finding(&mut self, _: &gents_protocol::operations::install_repair::Finding) {}
    fn step(
        &mut self,
        _: usize,
        _: usize,
        step: gents_protocol::operations::install_repair::RepairStep,
    ) {
        self.0.push(step);
    }
}

fn wire(actions: Vec<service::ServiceAction>) -> Vec<serde_json::Value> {
    use service::ServiceAction;
    actions
        .into_iter()
        .map(|action| {
            let (action, start_after, replace_program, answered) = match action {
                ServiceAction::MoveFrom {
                    start_after,
                    replace_program,
                    ..
                } => ("move", start_after, replace_program, false),
                ServiceAction::StopStale => ("stop", false, false, false),
                ServiceAction::Reinstall { start_after } => {
                    ("reinstall", start_after, false, false)
                }
                ServiceAction::SwitchProgram {
                    start_after,
                    answered,
                    ..
                } => ("switch", start_after, false, answered),
                ServiceAction::RefreshLog { restart } => ("refresh_log", restart, false, false),
                ServiceAction::Restart => ("restart", false, false, false),
                ServiceAction::RewriteDefinition => ("rewrite_definition", false, false, false),
            };
            json!({"action":action,"start_after":start_after,"replace_program":replace_program,"answered":answered})
        })
        .collect()
}

/// Runs the repair itself on each case, with no service manager: the first
/// service step it takes is recorded before it finds it has none, and a
/// repair that takes no service step finishes.
#[tokio::test]
async fn install_repair_settle_plans_match_lean() {
    for case in families::install_repair_settle_plans::cases::<
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    >() {
        let input = serde_json::to_value(&case.input).unwrap();
        let actions = settle_case(&input).await;
        case.check(serde_json::from_value(json!(wire(actions))).unwrap());
    }
}

/// The 2026-11-13 machine: no definition under the asking `HOME`, the user's
/// node held for the user's own home. The repair takes no service step.
#[tokio::test]
async fn install_repair_settle_beside_another_home_matches_lean() {
    for case in families::install_repair_settle_beside_another_home::cases::<
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    >() {
        let input = serde_json::to_value(&case.input).unwrap();
        let actions = settle_case(&input).await;
        case.check(serde_json::from_value(json!(wire(actions))).unwrap());
    }
}

async fn settle_case(input: &serde_json::Value) -> Vec<service::ServiceAction> {
    use super::port::{PortObservation, ServedIdentity};
    use gents_protocol::operations::install_repair::{HomeChoice, RepairStep, ServiceMove};
    use service::{NodeVersion, ServiceFacts};
    let program = super::ServiceProgram::new(super::current_program);
    let repair: RepairObservation = serde_json::from_value(input["repair"].clone()).unwrap();
    let observed = &input["service"];
    let bit = |field: &str| observed[field].as_bool().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("chosen");
    std::fs::create_dir_all(home.join("data")).unwrap();
    let facts = ServiceFacts {
        definition_home: bit("definition_present").then(|| {
            if bit("same_home") {
                home.clone()
            } else {
                temp.path().join("served")
            }
        }),
        executable_present: bit("executable_present"),
        log_current: bit("log_current"),
        job_loaded: bit("loaded"),
        job_running: bit("running"),
        job_failed: bit("failed"),
        was_serving: bit("was_serving"),
        home: repair.home,
        service_move: if bit("before_setup") {
            ServiceMove::BeforeSetup
        } else {
            ServiceMove::AfterSetup
        },
        node_version: Some(NodeVersion::Same),
        foreign_program: bit("foreign_program"),
        managed_update: bit("managed_update"),
        drain_supported: false,
        previous_version: None,
        port: match observed["serving"].as_str().unwrap() {
            "none" => PortObservation::NotListening,
            "same" => PortObservation::Gents {
                identity: ServedIdentity::Matches,
                payload: json!({}),
            },
            other => panic!("unexpected serving identity {other}"),
        },
        served_home_present: bit("served_home_present"),
        operation: repair.operation,
        holder: held(&observed["record"]),
    };
    assert_eq!(observed["version"], "same");
    assert_eq!(bit("home_complete"), repair.home.is_complete());
    let choice = HomeChoice {
        home,
        explicit: true,
    };
    let asked = super::Asked {
        interactive: repair.interactive,
        answer: repair.answer,
    };
    assert_eq!(
        serde_json::to_value(super::observe(&choice, &facts, repair.busy, asked)).unwrap(),
        input["repair"],
        "the case's two observations are of one machine"
    );
    let actions = super::service_actions(&planning::plan(&repair), &facts, &choice.home);
    let mut steps = Steps(Vec::new());
    let settled = super::settle(
        &choice,
        &facts,
        repair.busy,
        asked,
        None,
        None,
        None,
        &program,
        &mut steps,
    )
    .await;
    match actions.first() {
        None => {
            settled.unwrap();
            assert!(
                steps
                    .0
                    .iter()
                    .all(|step| *step == RepairStep::SetAsideFolder),
                "{:?}",
                steps.0
            );
        }
        Some(first) => {
            assert!(settled.is_err(), "{input}");
            assert_eq!(steps.0, [super::step_for(first)], "{input}");
        }
    }
    actions
}

#[test]
fn install_repair_client_identities_match_lean() {
    for case in families::install_repair_client_identities::cases::<String>() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let actual = crate::home_state::client_identity_action(&input);
        case.check(serde_json::from_value(serde_json::to_value(actual).unwrap()).unwrap());
    }
}

#[test]
fn install_repair_client_bindings_match_lean() {
    for case in families::install_repair_client_bindings::cases::<String>() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        use gents_desktop_core::home_reset::ClientBinding;
        case.check(
            match super::client::classify_binding(&input) {
                ClientBinding::Unbound => "unbound",
                ClientBinding::BoundToHome => "bound_to_home",
                ClientBinding::Orphaned => "orphaned",
            }
            .to_owned(),
        );
    }
}

#[test]
fn install_repair_store_exit_codes_match_lean() {
    for case in families::install_repair_store_exit_codes::cases::<String>() {
        let kind = serde_json::from_value(json!(case.input)).unwrap();
        let code = crate::native_service::incompatible_store_exit_code(kind);
        case.check(code.try_into().unwrap());
    }
}

#[test]
fn install_repair_crash_loops_match_lean() {
    use super::crash_loop::{CrashLoopWatch, JobSample};
    for case in families::install_repair_crash_loops::cases::<String>() {
        let mut watch = CrashLoopWatch::default();
        let mut states = Vec::new();
        for sample in &case.input {
            let exit = crate::native_service::ServiceExit {
                restarts: sample.counter,
                clean: sample.clean,
                code: Some(1),
                reason: "fixture exit".into(),
            };
            let event = match sample.kind.as_str() {
                "unloaded" => JobSample::Unloaded,
                "ready" => JobSample::Ready,
                "running" => JobSample::Running,
                "exited" => JobSample::Exited(&exit),
                other => panic!("unknown sample {other}"),
            };
            let looping = watch.observe(event).map(|(_, count)| count);
            states.push(json!({ "baseline": watch.baseline, "looping": looping }));
        }
        case.check(serde_json::from_value(json!(states)).unwrap());
    }
}

#[test]
fn install_repair_recovery_matches_lean() {
    for case in families::install_repair_recovery::cases() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        case.check(crate::service_update::can_restore_repair(&input));
    }
}
