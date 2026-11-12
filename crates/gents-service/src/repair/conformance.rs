use super::{home, planning, service};
use gents_lean_vocab::families;
use gents_protocol::operations::install_repair::{HomeState, RepairObservation};
use serde_json::json;

#[test]
fn install_repair_home_classification_matches_lean() {
    for case in families::install_repair_classification::cases::<String>() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let actual = home::classify_observation(&input);
        case.check(serde_json::from_value(serde_json::to_value(actual).unwrap()).unwrap());
    }
}

#[test]
fn install_repair_plans_match_lean() {
    for case in families::install_repair_plans::cases::<String, String, String, String>() {
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
    use gents_protocol::operations::install_repair::ServiceMove;
    use service::{NodeVersion, ServiceAction, ServiceFacts};
    use std::path::PathBuf;
    for case in families::install_repair_service_plans::cases::<String, String, String>() {
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
        };
        let actual: Vec<_> = service::plan_service(&facts, &home).into_iter().map(|action| {
            let (action, start_after, replace_program, answered) = match action {
                ServiceAction::MoveFrom { start_after, replace_program, .. } => ("move",start_after,replace_program,false),
                ServiceAction::StopStale => ("stop",false,false,false),
                ServiceAction::Reinstall { start_after } => ("reinstall",start_after,false,false),
                ServiceAction::SwitchProgram { start_after, answered, .. } => ("switch",start_after,false,answered),
                ServiceAction::RefreshLog { restart } => ("refresh_log",restart,false,false),
                ServiceAction::Restart => ("restart",false,false,false),
            };
            json!({"action":action,"start_after":start_after,"replace_program":replace_program,"answered":answered})
        }).collect();
        case.check(serde_json::from_value(json!(actual)).unwrap());
    }
}

#[test]
fn install_repair_client_bindings_match_lean() {
    for case in families::install_repair_client_bindings::cases::<String>() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let actual = super::client::classify_binding(&input);
        case.check(serde_json::from_value(serde_json::to_value(actual).unwrap()).unwrap());
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
