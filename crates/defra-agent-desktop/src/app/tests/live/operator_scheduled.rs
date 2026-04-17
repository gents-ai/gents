use super::*;

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_operator_scheduled_task_and_failures() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-scheduled", global_log_store())?;
    let docs = fixture.docs.clone();

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Operator);
        assert_operator_filter_round_trip(
            driver,
            OperatorSection::ScheduledTasks,
            "Live Audit Scheduled Task",
            &docs.scheduled_task_id,
            "definitely-missing-live-task",
        )?;
        driver.click_target(&audit::targets::operator_entity(&docs.scheduled_task_id));

        driver.scroll_right_rail_until_target(
            "live scheduled task interval field",
            &audit::targets::operator_field("Interval Secs"),
        )?;
        driver.replace_text_in_target(&audit::targets::operator_field("Interval Secs"), "0");
        driver.scroll_right_rail_until_target(
            "live scheduled task apply validation",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        assert!(driver
            .app
            .state
            .operator
            .last_apply_error
            .as_deref()
            .is_some_and(|error| error.contains("interval_secs must be greater than zero")));
        let validation_texts = wait_for_value(
            "live scheduled validation error rendered",
            Duration::from_secs(2),
            || {
                let texts = driver.render();
                texts
                    .iter()
                    .any(|text| text.contains("interval_secs must be greater than zero"))
                    .then_some(texts)
            },
        )?;
        assert!(validation_texts
            .iter()
            .any(|text| text.contains("interval_secs must be greater than zero")));

        driver.replace_text_in_target(&audit::targets::operator_field("Interval Secs"), "120");
        driver.replace_text_in_target(
            &audit::targets::operator_field("Name"),
            "Live Scheduled Review",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Prompt"),
            "Run a live scheduled desktop audit.",
        );
        driver.scroll_right_rail_until_target(
            "live scheduled task enabled toggle",
            &audit::targets::operator_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.scroll_right_rail_until_target(
            "live scheduled task next-run field",
            &audit::targets::operator_field("Next Run At"),
        )?;
        driver.replace_text_in_target(
            &audit::targets::operator_field("Next Run At"),
            "2035-04-15T12:34:56Z",
        );
        driver.scroll_right_rail_until_target(
            "live scheduled task apply",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live scheduled task edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .filter(|row| {
                            row.interval_secs == Some(120)
                                && row.name.as_deref() == Some("Live Scheduled Review")
                                && row.prompt.as_deref()
                                    == Some("Run a live scheduled desktop audit.")
                                && row.enabled == Some(false)
                                && row.next_run_at.as_deref() == Some("2035-04-15T12:34:56Z")
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_entity(&docs.scheduled_task_id));
        driver.scroll_right_rail_until_target(
            "live scheduled task re-enable toggle",
            &audit::targets::operator_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.scroll_right_rail_until_target(
            "live scheduled task re-enable apply",
            audit::targets::OPERATOR_APPLY,
        )?;
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live scheduled task re-enabled",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .filter(|row| row.enabled == Some(true))
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_entity(&docs.scheduled_task_id));
        let prior_next_run = driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .scheduled_tasks
                    .iter()
                    .find(|row| row.task_id == docs.scheduled_task_id)
                    .and_then(|row| row.next_run_at.clone())
            })
            .ok_or_else(|| anyhow!("missing live scheduled task next_run_at"))?;
        driver.scroll_right_rail_until_target(
            "live scheduled task run-now button",
            audit::targets::OPERATOR_RUN_NOW,
        )?;
        driver.click_target(audit::targets::OPERATOR_RUN_NOW);
        wait_for_value(
            "live scheduled task run-now persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .and_then(|row| row.next_run_at.clone())
                        .filter(|next_run_at| next_run_at != &prior_next_run)
                })
            },
        )?;

        let failed_task = driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .scheduled_tasks
                    .iter()
                    .find(|row| row.task_id == docs.scheduled_task_id)
                    .cloned()
            })
            .ok_or_else(|| anyhow!("missing live scheduled task before failure insert"))?;
        let mut failed_task = failed_task;
        failed_task.last_status = Some("error".to_string());
        failed_task.last_error = Some("live scheduled audit failure".to_string());
        failed_task.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        let failed_task_agent_did = failed_task.agent_did.clone();
        let client = Arc::clone(
            driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        driver
            .app
            .runtime
            .block_on(client.save_scheduled_task(&failed_task))?;
        wait_for_value(
            "live scheduled failure persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .scheduled_tasks
                        .iter()
                        .find(|row| row.task_id == docs.scheduled_task_id)
                        .filter(|row| {
                            row.last_status.as_deref() == Some("error")
                                && row.last_error.as_deref() == Some("live scheduled audit failure")
                        })
                        .map(|row| row.task_id.clone())
                })
            },
        )?;

        driver.app.state.operator.selected_agent_did = failed_task_agent_did;
        driver.app.state.operator.selected_entity_id = None;
        driver.app.state.operator.draft = None;
        driver.app.state.operator.draft_source_entity_id = None;
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RecentFailures,
        ));
        let failure_id = format!("task:{}", docs.scheduled_task_id);
        driver.wait_for_target(
            "live scheduled task failure row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&failure_id),
        )?;
        let failure_texts = driver.click_target(&audit::targets::operator_entity(&failure_id));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("Failure Detail")));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("live scheduled audit failure")));
    }

    fixture.shutdown()
}

