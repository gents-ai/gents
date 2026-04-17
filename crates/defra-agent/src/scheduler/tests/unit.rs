use super::super::*;

#[test]
fn scheduled_task_parses_from_json() {
    let json = serde_json::json!({
        "_docID": "abc123",
        "task_id": "seed-fleet-health",
        "name": "fleet-health-daily",
        "behavior_id": "amy-general",
        "prompt": "Check fleet health",
        "interval_secs": 86400,
        "enabled": true,
        "next_run_at": null,
        "run_count": 0,
    });

    let task = ScheduledTask {
        doc_id: json["_docID"].as_str().unwrap_or_default().to_string(),
        task_id: json["task_id"].as_str().unwrap_or_default().to_string(),
        name: json["name"].as_str().unwrap_or_default().to_string(),
        behavior_id: json["behavior_id"].as_str().unwrap_or_default().to_string(),
        prompt: json["prompt"].as_str().unwrap_or_default().to_string(),
        interval_secs: json["interval_secs"].as_i64().unwrap_or(3600),
        enabled: json["enabled"].as_bool().unwrap_or(false),
        next_run_at: json["next_run_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc)),
        run_count: json["run_count"].as_i64().unwrap_or(0),
    };

    assert_eq!(task.doc_id, "abc123");
    assert_eq!(task.name, "fleet-health-daily");
    assert_eq!(task.interval_secs, 86400);
    assert!(task.next_run_at.is_none());
    assert_eq!(task.run_count, 0);
}

#[test]
fn runtime_context_format() {
    let now = "2026-04-02T14:30:00Z";
    let host = "studio-1";
    let name = "fleet-health-daily";
    let run_count = 5;

    let context = format!(
        "Current time: {}\nHost: {}\nTask: {} (run #{})\n\n",
        now,
        host,
        name,
        run_count + 1,
    );

    assert!(context.contains("Current time: 2026-04-02T14:30:00Z"));
    assert!(context.contains("Host: studio-1"));
    assert!(context.contains("Task: fleet-health-daily (run #6)"));
}

#[test]
fn scheduled_task_from_value_parses() {
    let json = serde_json::json!({
        "_docID": "doc1",
        "task_id": "task-1",
        "name": "test-task",
        "behavior_id": "general",
        "prompt": "Do something",
        "interval_secs": 3600,
        "enabled": true,
        "next_run_at": null,
        "run_count": 3,
    });

    let task = ScheduledTask::from_value(&json).expect("should parse");
    assert_eq!(task.doc_id, "doc1");
    assert_eq!(task.task_id, "task-1");
    assert_eq!(task.name, "test-task");
    assert_eq!(task.behavior_id, "general");
    assert_eq!(task.prompt, "Do something");
    assert_eq!(task.interval_secs, 3600);
    assert!(task.enabled);
    assert!(task.next_run_at.is_none());
    assert_eq!(task.run_count, 3);
}

#[test]
fn scheduled_task_from_value_rejects_missing_behavior_id() {
    let json = serde_json::json!({
        "_docID": "doc2",
        "task_id": "task-2",
        "name": "timed-task",
        "prompt": "Run check",
        "interval_secs": 600,
        "enabled": true,
        "next_run_at": "2026-04-02T14:30:00Z",
        "run_count": 1,
    });

    let error = ScheduledTask::from_value(&json).expect_err("should reject");
    assert!(error.to_string().contains("behavior_id"));
}

#[test]
fn scheduled_task_from_value_rejects_invalid_timestamp() {
    let json = serde_json::json!({
        "_docID": "doc3",
        "task_id": "task-3",
        "name": "bad-timestamp",
        "behavior_id": "code",
        "prompt": "Run check",
        "interval_secs": 600,
        "enabled": true,
        "next_run_at": "not-a-time",
        "run_count": 1,
    });

    let error = ScheduledTask::from_value(&json).expect_err("should reject");
    assert!(error.to_string().contains("next_run_at"));
}

#[test]
fn scheduled_task_from_value_with_timestamp() {
    let json = serde_json::json!({
        "_docID": "doc4",
        "task_id": "task-4",
        "name": "timed-task",
        "behavior_id": "general",
        "prompt": "Run check",
        "interval_secs": 600,
        "enabled": true,
        "next_run_at": "2026-04-02T14:30:00Z",
        "run_count": 1,
    });

    let task = ScheduledTask::from_value(&json).expect("should parse");
    assert!(task.next_run_at.is_some());
    let next = task.next_run_at.unwrap();
    assert_eq!(
        next.to_rfc3339_opts(SecondsFormat::Secs, true),
        "2026-04-02T14:30:00Z"
    );
}

#[test]
fn is_due_when_never_run() {
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: true,
        next_run_at: None,
        run_count: 0,
    };
    assert!(task.is_due());
}

#[test]
fn is_due_when_past() {
    let past = Utc::now() - chrono::Duration::seconds(10);
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: true,
        next_run_at: Some(past),
        run_count: 1,
    };
    assert!(task.is_due());
}

#[test]
fn not_due_when_future() {
    let future = Utc::now() + chrono::Duration::seconds(3600);
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: true,
        next_run_at: Some(future),
        run_count: 1,
    };
    assert!(!task.is_due());
}

#[test]
fn task_timeout_is_fifteen_minutes() {
    assert_eq!(TASK_TIMEOUT_SECS, 900);
}

#[test]
fn not_due_when_disabled() {
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: false,
        next_run_at: None,
        run_count: 0,
    };
    assert!(!task.is_due());
}

#[test]
fn missing_scheduled_task_collection_is_treated_as_empty() {
    assert!(is_missing_scheduled_task_collection_error(
        "query ScheduledTask failed: [QueryResponseError { message: \"collection not found: Cannot query collection 'ScheduledTask': collection not found\", path: None, locations: None }]"
    ));
    assert!(!is_missing_scheduled_task_collection_error(
        "query ScheduledTask failed: some other datastore error"
    ));
}
