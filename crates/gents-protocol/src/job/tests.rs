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
            "not a plugin job; find the job's address with `gents job list`",
        ),
        (
            JobReadError::NotAuthorized,
            "this job belongs to another session or requester; read it from the session that started it",
        ),
        (
            JobReadError::NotYetObservable,
            "no job row is visible at this address yet; retry shortly, or check the address with `gents job list`",
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
