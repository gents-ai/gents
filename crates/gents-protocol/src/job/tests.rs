use super::*;
use serde_json::json;

fn decode(value: serde_json::Value) -> Result<JobStepOutput, serde_json::Error> {
    serde_json::from_value(value)
}

fn effects_of(output: JobStepOutput) -> JobEffects {
    match output {
        JobStepOutput::Effects { effects } => effects,
        other => panic!("expected an effects output, got {other:?}"),
    }
}

fn assert_lists_empty(e: &JobEffects) {
    assert!(e.spawn.is_empty() && e.message.is_empty() && e.interrupt.is_empty());
    assert!(e.writes.is_empty() && e.scratch.is_empty());
}

#[test]
fn effects_lists_default_to_empty_when_absent() {
    let e = effects_of(decode(json!({ "effects": {} })).expect("decode"));
    assert_lists_empty(&e);
    assert_eq!((e.pin, e.resume, e.progress), (None, None, None));
}

#[test]
fn effects_lists_default_to_empty_when_null() {
    let e = effects_of(
        decode(json!({ "effects": {
            "spawn": null, "message": null, "interrupt": null,
            "writes": null, "scratch": null
        } }))
        .expect("decode"),
    );
    assert_lists_empty(&e);
}

#[test]
fn effects_decode_one_list_alone() {
    let e = effects_of(
        decode(json!({ "effects": { "interrupt": [{ "slot": "0:a" }] } })).expect("decode"),
    );
    assert_eq!(
        e.interrupt,
        vec![JobInterrupt {
            slot: "0:a".to_string()
        }]
    );
    assert!(e.spawn.is_empty() && e.message.is_empty());
    assert!(e.writes.is_empty() && e.scratch.is_empty());
}

#[test]
fn empty_effects_lists_are_omitted_on_encode() {
    let output = decode(json!({ "effects": { "message": [], "writes": [] } })).expect("decode");
    assert_eq!(
        serde_json::to_value(&output).expect("encode"),
        json!({ "effects": {} })
    );
}

#[test]
fn guest_state_rides_on_effects_and_suspend_only() {
    let effects = effects_of(decode(json!({"effects": {"state": {"phase": "scan"}}})).unwrap());
    assert_eq!(effects.state, Some(json!({"phase": "scan"})));
    let suspend =
        decode(json!({"suspend": {"kind": "pause", "message": "m"}, "state": [1, 2]})).unwrap();
    assert!(
        matches!(&suspend, JobStepOutput::Suspend { state, .. } if *state == Some(json!([1, 2])))
    );
    assert_eq!(
        serde_json::to_value(&suspend).unwrap()["state"],
        json!([1, 2])
    );
    assert!(
        decode(json!({"result": {}, "state": 1})).is_err(),
        "a terminal result carries no state for a next step"
    );
}

#[test]
fn unknown_effects_key_is_still_refused() {
    assert!(decode(json!({ "effects": { "spawns": [] } })).is_err());
}

fn progress_json() -> serde_json::Value {
    json!({
        "title": "t", "state": "running", "phases": [], "active": [], "spawned": 0,
        "budget": { "total": 1, "spent": 0, "remaining": 1 },
        "last_event": "e", "observed_terminal_children": 0, "revision": 0
    })
}

#[test]
fn progress_last_event_at_is_optional() {
    let absent: JobProgress = serde_json::from_value(progress_json()).expect("absent");
    assert_eq!(absent.last_event_at, None);
    let mut null = progress_json();
    null["last_event_at"] = serde_json::Value::Null;
    let null: JobProgress = serde_json::from_value(null).expect("null");
    assert_eq!(null.last_event_at, None);
    let mut stamped = progress_json();
    stamped["last_event_at"] = json!("2026-10-14T21:24:00Z");
    let stamped: JobProgress = serde_json::from_value(stamped).expect("stamped");
    assert_eq!(
        stamped.last_event_at.as_deref(),
        Some("2026-10-14T21:24:00Z")
    );
    assert_eq!(
        serde_json::to_value(&absent).expect("encode"),
        progress_json()
    );
}

#[test]
fn job_read_error_display_names_the_next_call() {
    let message = || "m".to_owned();
    let cases = [
        (
            JobReadError::NotAJob,
            "not a plugin job; find the job's address with `gents automation job list`",
        ),
        (
            JobReadError::NotAuthorized,
            "this job belongs to another session or requester; read it from the session that started it",
        ),
        (
            JobReadError::PermissionDenied,
            "access to the job was denied; check this client's permissions with the runtime owner",
        ),
        (
            JobReadError::NotYetObservable,
            "no job row is visible at this address yet; retry shortly, or check the address with `gents automation job list`",
        ),
        (
            JobReadError::Unavailable { message: message() },
            "job store unavailable: m",
        ),
        (
            JobReadError::Malformed { message: message() },
            "job row does not decode: m; retrying will not help",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
    }
    let boxed: Box<dyn std::error::Error + Send + Sync> = JobReadError::NotAJob.into();
    assert_eq!(
        boxed.downcast_ref::<JobReadError>(),
        Some(&JobReadError::NotAJob)
    );
}

fn round_trip<T>(name: &str, value: &serde_json::Value) -> (T, serde_json::Value)
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let decoded: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("{name}: strict decode: {error}"));
    let encoded =
        serde_json::to_value(&decoded).unwrap_or_else(|error| panic!("{name}: encode: {error}"));
    assert_eq!(&encoded, value, "{name}: re-encoded JSON");
    (decoded, encoded)
}

fn collect_field_names(value: &serde_json::Value, names: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, nested) in map {
                names.insert(name.clone());
                collect_field_names(nested, names);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_field_names(item, names);
            }
        }
        _ => {}
    }
}

/// `strip_secret_fields` drops a field whose upper-cased name contains one of
/// these (`documents::callback::is_secret_env_name`), so no job field may.
const SECRET_SUBSTRINGS: [&str; 4] = ["KEY", "SECRET", "TOKEN", "PASSWORD"];

fn data_query() -> DataQuery {
    DataQuery {
        collection: "c".to_owned(),
        filter: serde_json::Map::new(),
        order: Some(serde_json::Map::new()),
        limit: 1,
        offset: Some(0),
        fields: vec!["f".to_owned()],
    }
}

/// Every wire sample the Lean types render decodes strictly into its
/// `gents_protocol::job` type, re-encodes to the same JSON and carries what
/// the model says it carries; no job field name is one the secret filter drops.
#[test]
fn plugin_job_wire_samples_round_trip_and_are_secret_free() {
    let names = std::cell::RefCell::new(std::collections::BTreeSet::new());
    gents_lean_vocab::families::plugin_job_wire_sample::replay::<JobState, JobOrigin, String>(
        |i| {
            let value: serde_json::Value = serde_json::from_str(&i.value)
                .unwrap_or_else(|error| panic!("{}: sample is JSON: {error}", i.name));
            let none = gents_lean_vocab::families::plugin_job_wire_sample::WireObservation {
                origin: None,
                echo_kind: None,
                root_call: false,
                jobs: Vec::new(),
                pin: false,
                resume: false,
                code: None,
            };
            let (observation, encoded) = match i.kind.as_str() {
                "job_step_input" => {
                    let (input, encoded) = round_trip::<JobStepInput>(&i.name, &value);
                    let echo_kind = input.control.as_ref().map(|echo| {
                        serde_json::to_value(echo.kind)
                            .expect("echo kind encodes")
                            .as_str()
                            .expect("echo kind is a string")
                            .to_owned()
                    });
                    (
                        gents_lean_vocab::families::plugin_job_wire_sample::WireObservation {
                            origin: input.origin,
                            echo_kind,
                            root_call: input.root_call.is_some(),
                            jobs: input.jobs.iter().map(|row| row.state).collect(),
                            pin: input.pin.is_some(),
                            ..none
                        },
                        encoded,
                    )
                }
                "job_step_output" => {
                    let (output, encoded) = round_trip::<JobStepOutput>(&i.name, &value);
                    let (pin, resume) = match &output {
                        JobStepOutput::Result { pin, resume, .. }
                        | JobStepOutput::Suspend { pin, resume, .. } => {
                            (pin.is_some(), resume.is_some())
                        }
                        JobStepOutput::Effects { effects } => {
                            (effects.pin.is_some(), effects.resume.is_some())
                        }
                        JobStepOutput::Fail { .. } => (false, false),
                    };
                    (
                        gents_lean_vocab::families::plugin_job_wire_sample::WireObservation {
                            pin,
                            resume,
                            ..none
                        },
                        encoded,
                    )
                }
                "job_view" => {
                    let (view, encoded) = round_trip::<JobView>(&i.name, &value);
                    (
                        gents_lean_vocab::families::plugin_job_wire_sample::WireObservation {
                            origin: Some(view.pin.origin),
                            code: view.outcome.map(|outcome| outcome.code),
                            ..none
                        },
                        encoded,
                    )
                }
                "job_outcome" => {
                    let (outcome, encoded) = round_trip::<JobOutcome>(&i.name, &value);
                    (
                        gents_lean_vocab::families::plugin_job_wire_sample::WireObservation {
                            code: Some(outcome.code),
                            ..none
                        },
                        encoded,
                    )
                }
                other => panic!("{}: unknown wire kind {other}", i.name),
            };
            collect_field_names(&encoded, &mut names.borrow_mut());
            observation
        },
    );
    let mut names = names.into_inner();
    let settings: PluginJobSettings = serde_json::from_value(json!({})).expect("settings defaults");
    for constructed in [
        serde_json::to_value(JobPolicy::default()),
        serde_json::to_value(settings),
        serde_json::to_value(JobControlRequest {
            row_doc_id: "row".to_owned(),
            kind: JobControlKind::Stop,
        }),
        serde_json::to_value(JobResultRequest {
            row_doc_id: "row".to_owned(),
            offset: 0,
            limit: 1,
        }),
        serde_json::to_value(JobResultResponse {
            row_doc_id: "row".to_owned(),
            state: JobState::Running,
            result: "text".to_owned(),
            offset: 0,
            total_bytes: 4,
            truncated: true,
        }),
        serde_json::to_value(ChildOutputRequest {
            slots: vec!["1:a".to_owned()],
            offset: 0,
            limit: 1,
        }),
        serde_json::to_value(ChildOutputResponse {
            slots: std::collections::BTreeMap::from([(
                "1:a".to_owned(),
                ChildOutputSlice {
                    text: "text".to_owned(),
                    truncated: true,
                },
            )]),
        }),
        serde_json::to_value(DataCall {
            id: "d1".to_owned(),
            query: data_query(),
        }),
        serde_json::to_value(DataQueryResponse {
            rows: vec![serde_json::Map::new()],
        }),
        serde_json::to_value(JobCall::ChildOutput {
            id: "c1".to_owned(),
            child_output: ChildOutputRequest {
                slots: Vec::new(),
                offset: 0,
                limit: 1,
            },
        }),
        serde_json::to_value(JobCall::Result {
            id: "c2".to_owned(),
            result: JobResultRequest {
                row_doc_id: "row".to_owned(),
                offset: 0,
                limit: 1,
            },
        }),
        serde_json::to_value(RoundResult::Result { result: 1 }),
        serde_json::to_value(RoundResult::<u32>::Error {
            error: "e".to_owned(),
        }),
        serde_json::to_value(JobControlOutcome::Applied {
            state: JobState::Running,
            killed_children: 1,
        }),
        serde_json::to_value(JobControlOutcome::AlreadyTerminal {
            state: JobState::Running,
        }),
        serde_json::to_value(JobReadError::NotAJob),
        serde_json::to_value(JobReadError::Unavailable {
            message: "m".to_owned(),
        }),
        serde_json::to_value(JobReadError::Malformed {
            message: "m".to_owned(),
        }),
        serde_json::to_value(PluginJobDeclaration(true)),
        serde_json::to_value(PluginDataDeclaration {
            collections: vec!["c".to_owned()],
            ops: vec!["query".to_owned()],
            stamped: std::collections::BTreeMap::from([("f".to_owned(), StampedField::CallerDid)]),
        }),
    ] {
        collect_field_names(&constructed.expect("constructed value encodes"), &mut names);
    }
    assert!(
        names.contains("row_doc_id") && names.contains("trusted_packs"),
        "the walk reaches nested and defaulted fields: {names:?}"
    );
    for name in &names {
        let upper = name.to_ascii_uppercase();
        for needle in SECRET_SUBSTRINGS {
            assert!(
                !upper.contains(needle),
                "job field {name:?} contains {needle}"
            );
        }
    }
}
