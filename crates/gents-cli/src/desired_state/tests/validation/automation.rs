use super::super::*;

#[test]
fn validate_rejects_empty_task_id() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("");
    task.task_id = String::new();
    manifest.tasks.push(task);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("empty task_id")),
        "expected empty task_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_empty_task_behavior_id() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.behavior_id = String::new();
    manifest.tasks.push(task);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("summarize-inbox") && message.contains("behavior_id")),
        "expected empty behavior_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_task_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    manifest.tasks.push(sample_task("summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("duplicate task_id") && message.contains("summarize-inbox")
        }),
        "expected duplicate task_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_empty_schedule_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("", "summarize-inbox");
    schedule.schedule_id = String::new();
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("empty schedule_id")),
        "expected empty schedule_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_schedule_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("duplicate schedule_id") && message.contains("hourly")
        }),
        "expected duplicate schedule_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_interval_zero_or_negative() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("hourly", "summarize-inbox");
    schedule.interval_secs = Some(0);
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|message| message.contains("hourly") && message.contains("interval_secs")),
        "expected interval_secs >= 1 rejection, got {errors:?}"
    );
}

#[test]
fn validate_accepts_cron_schedule_with_timezone() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("weekday-digest", "summarize-inbox");
    schedule.interval_secs = None;
    schedule.cron = Some("30 3 * * MON".to_string());
    schedule.timezone = Some("America/Los_Angeles".to_string());
    schedule.missed_run_policy = Some("latest_only".to_string());
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors.is_empty(),
        "expected valid cron schedule, got {errors:?}"
    );
}

#[test]
fn validate_rejects_malformed_cron_schedule() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("bad-cron", "summarize-inbox");
    schedule.interval_secs = None;
    schedule.cron = Some("30 3 * *".to_string());
    schedule.timezone = Some("UTC".to_string());
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("bad-cron")
                && message.contains("invalid cron schedule")
                && message.contains("exactly 5 fields")
        }),
        "expected malformed cron rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_invalid_cron_timezone() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("bad-zone", "summarize-inbox");
    schedule.interval_secs = None;
    schedule.cron = Some("30 3 * * MON".to_string());
    schedule.timezone = Some("Mars/Olympus".to_string());
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("bad-zone")
                && message.contains("invalid cron schedule")
                && message.contains("invalid IANA timezone")
        }),
        "expected invalid timezone rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_with_interval_and_cron() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("double-cadence", "summarize-inbox");
    schedule.cron = Some("30 3 * * MON".to_string());
    schedule.timezone = Some("UTC".to_string());
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("double-cadence")
                && message.contains("exactly one of interval_secs or cron")
        }),
        "expected double cadence rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_unknown_concurrency() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut schedule = sample_schedule("hourly", "summarize-inbox");
    schedule.concurrency = "everything-everywhere".to_string();
    manifest.schedules.push(schedule);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("concurrency")
                && message.contains("everything-everywhere")
        }),
        "expected unknown concurrency rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_task_unknown_behavior() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.behavior_id = "did:test:test:missing".to_string();
    manifest.tasks.push(task);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("summarize-inbox")
                && message.contains("missing")
                && message.contains("behavior_id")
        }),
        "expected missing behavior_id reference rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_task_template_referencing_doc_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "Schedule fired at {{ event.fired_at }} for {{ doc.foo }}.".to_string();
    manifest.tasks.push(task);
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("forbidden scope")
                && message.contains("doc")
                && message.contains("event.*")
        }),
        "expected schedule-scope rejection for doc.*, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_task_template_referencing_args_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "{{ args.target }}".to_string();
    manifest.tasks.push(task);
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("forbidden scope")
                && message.contains("args")
        }),
        "expected schedule-scope rejection for args.*, got {errors:?}"
    );
}

#[test]
fn validate_accepts_schedule_task_template_using_only_event_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "Run at {{ event.fired_at }} for {{ event.trigger_kind }}.".to_string();
    manifest.tasks.push(task);
    manifest
        .schedules
        .push(sample_schedule("hourly", "summarize-inbox"));

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|message| message.contains("forbidden scope")),
        "expected no schedule-scope rejections, got {errors:?}"
    );
}

#[test]
fn validate_rejects_schedule_unknown_task() {
    let mut manifest = manifest_with_default_behavior();
    manifest
        .schedules
        .push(sample_schedule("hourly", "missing-task"));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("hourly")
                && message.contains("missing-task")
                && message.contains("task_id")
        }),
        "expected missing task_id reference rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_referencing_unknown_task() {
    let mut manifest = manifest_with_default_behavior();
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "missing-task",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet")
                && message.contains("unknown task_id")
                && message.contains("missing-task")
        }),
        "expected unknown task_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_unknown_event_kind() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut trigger = sample_event_trigger_for("new-customer-greet", "summarize-inbox");
    trigger.event_kind = "updated".to_string();
    manifest.event_triggers.push(trigger);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet") && message.contains("unsupported event_kind")
        }),
        "expected unsupported event_kind rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_template_referencing_args_scope() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "{{ args.foo }}".to_string();
    manifest.tasks.push(task);
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet") && message.contains("forbidden scope: args")
        }),
        "expected event-trigger forbidden-args rejection, got {errors:?}"
    );
}

#[test]
fn validate_accepts_event_trigger_template_using_event_and_doc_scopes() {
    let mut manifest = manifest_with_default_behavior();
    let mut task = sample_task("summarize-inbox");
    task.prompt_template = "{{ event.fired_at }} {{ doc.name }}".to_string();
    manifest.tasks.push(task);
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|message| message.contains("forbidden scope")),
        "expected no forbidden-scope rejections for event+doc scopes, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_event_trigger_id() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));
    manifest.event_triggers.push(sample_event_trigger_for(
        "new-customer-greet",
        "summarize-inbox",
    ));

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("duplicate") && message.contains("new-customer-greet")
        }),
        "expected duplicate trigger_id rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_event_trigger_unknown_concurrency() {
    let mut manifest = manifest_with_default_behavior();
    manifest.tasks.push(sample_task("summarize-inbox"));
    let mut trigger = sample_event_trigger_for("new-customer-greet", "summarize-inbox");
    trigger.concurrency = "weird".to_string();
    manifest.event_triggers.push(trigger);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|message| {
            message.contains("new-customer-greet")
                && message.contains("unknown concurrency")
                && message.contains("weird")
        }),
        "expected unknown concurrency rejection, got {errors:?}"
    );
}
