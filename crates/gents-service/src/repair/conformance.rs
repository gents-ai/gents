use super::{home, planning, service};
use gents_lean_vocab::families;
use gents_protocol::operations::install_repair::{HomeState, RepairObservation};
use serde_json::json;

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

#[test]
fn install_repair_service_plans_match_lean() {
    use super::port::{PortObservation, ServedIdentity};
    use gents_protocol::operations::install_repair::{RepairOperation, ServiceMove};
    use service::{NodeVersion, ServiceFacts};
    use std::path::PathBuf;
    for case in families::install_repair_service_plans::cases::<String, String, String, String>() {
        let input = &case.input;
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
    use super::port::{PortObservation, ServedIdentity};
    use gents_protocol::operations::install_repair::{HomeChoice, RepairStep, ServiceMove};
    use service::{NodeVersion, ServiceFacts};
    let program = super::ServiceProgram::new(super::current_program);
    for case in families::install_repair_settle_plans::cases::<
        String,
        String,
        String,
        String,
        String,
        String,
    >() {
        let input = serde_json::to_value(&case.input).unwrap();
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
        case.check(serde_json::from_value(json!(wire(actions))).unwrap());
    }
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
