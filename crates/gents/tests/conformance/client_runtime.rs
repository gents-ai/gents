use super::*;

fn readiness_reason(code: &str) -> gents_protocol::row::BehaviorReadinessUnavailableReason {
    use gents_protocol::row::BehaviorReadinessUnavailableReason as Reason;
    match code {
        "behavior_disabled" => Reason::BehaviorDisabled,
        "runtime_configuration_invalid" => Reason::RuntimeConfigurationInvalid,
        "backend_not_configured" => Reason::BackendNotConfigured,
        "backend_disabled" => Reason::BackendDisabled,
        "backend_temporarily_unavailable" => Reason::BackendTemporarilyUnavailable,
        "credentials_required" => Reason::CredentialsRequired,
        "inference_profile_invalid" => Reason::InferenceProfileInvalid,
        "tool_configuration_invalid" => Reason::ToolConfigurationInvalid,
        "tool_surface_unavailable" => Reason::ToolSurfaceUnavailable,
        other => panic!("unknown Lean readiness reason {other}"),
    }
}

fn readiness_reason_code<T: serde::Serialize>(reason: T) -> String {
    serde_json::to_value(reason)
        .expect("serialize readiness reason")
        .as_str()
        .expect("readiness reason serializes as string")
        .to_string()
}

#[test]
fn generated_behavior_readiness_cases_drive_the_production_projector() {
    use gents_protocol::row::{
        project_behavior_readiness, project_behavior_readiness_source, AgentBehaviorReadinessRow,
        BehaviorReadinessProcessState, BehaviorReadinessSourceEntry, ProjectedBehaviorReadiness,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };

    let cases = lean_client_behavior_readiness_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let runtime_reason = readiness_reason(&case.runtime_unavailable_reason);
        let process_state = serde_json::from_value::<BehaviorReadinessProcessState>(
            serde_json::Value::String(case.process_state.clone()),
        )
        .unwrap_or_else(|error| panic!("{} invalid generated process state: {error}", case.name));
        let selected_assigned = case.runnable || case.unavailable || case.startup_demoted;
        let default_behavior_id = if selected_assigned { "20" } else { "10" };
        // Behavior "20" is the only behavior the generated cases assign; the
        // unassigned rows keep a valid default ("10") while "20" stays absent
        // from the runtime projection.
        let sources = vec![BehaviorReadinessSourceEntry {
            behavior_id: default_behavior_id.to_string(),
            dispatcher_present: if selected_assigned {
                case.runnable
            } else {
                true
            },
            unavailable_reason: if selected_assigned && case.unavailable {
                Some(runtime_reason)
            } else {
                None
            },
            startup_demoted: selected_assigned && case.startup_demoted,
        }];
        let mut snapshot = project_behavior_readiness_source(
            process_state,
            case.active_generation,
            case.router_generation,
            default_behavior_id,
            sources,
        )
        .unwrap_or_else(|error| panic!("{} invalid generated source: {error}", case.name));
        if case.observation_kind == "unsupported_version" {
            snapshot.format_version = BEHAVIOR_READINESS_FORMAT_VERSION + 1;
        }
        let row = AgentBehaviorReadinessRow {
            agent_did: "did:test:lean-readiness".to_string(),
            snapshot_json: if case.observation_kind == "malformed" {
                "not-json".to_string()
            } else {
                serde_json::to_string(&snapshot).unwrap()
            },
            updated_at: "2026-08-28T00:00:00Z".to_string(),
        };
        let projection = project_behavior_readiness(
            case.observation_present.then_some(&row),
            "did:test:lean-readiness",
            ["20"],
            Some("20"),
            chrono::DateTime::parse_from_rfc3339("2026-08-28T00:00:10Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let projected = projection
            .behaviors
            .get("20")
            .unwrap_or_else(|| panic!("{} missing selected behavior", case.name));
        // The generated model projects semantic runtime state, not an
        // observation lease. Idle age and clock skew cannot change its answer.
        for timestamp in ["1970-01-01T00:00:00Z", "2999-01-01T00:00:00Z"] {
            let mut aged_row = row.clone();
            aged_row.updated_at = timestamp.to_string();
            let aged = project_behavior_readiness(
                case.observation_present.then_some(&aged_row),
                "did:test:lean-readiness",
                ["20"],
                Some("20"),
                chrono::DateTime::parse_from_rfc3339("2026-08-28T00:00:10Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            );
            assert_eq!(
                aged.behaviors, projection.behaviors,
                "{}: {timestamp}",
                case.name
            );
            assert_eq!(
                aged.unknown_reason, projection.unknown_reason,
                "{}: {timestamp}",
                case.name
            );
        }
        let (state, reason) = match projected {
            ProjectedBehaviorReadiness::Ready => ("ready", None),
            ProjectedBehaviorReadiness::Unavailable(reason) => {
                ("unavailable", Some(readiness_reason_code(*reason)))
            }
            ProjectedBehaviorReadiness::Unknown(reason) => {
                ("unknown", Some(readiness_reason_code(*reason)))
            }
        };
        assert_eq!(state, case.expected_state, "{} state", case.name);
        assert_eq!(reason, case.expected_reason, "{} reason", case.name);
        if case.observation_kind == "observed" {
            assert_eq!(
                state == "ready",
                case.expected_runtime_admissible,
                "{} canonical observation/runtime admission parity",
                case.name
            );
        } else {
            assert_eq!(
                state, "unknown",
                "{} transport failure closes client",
                case.name
            );
        }
    }
}
