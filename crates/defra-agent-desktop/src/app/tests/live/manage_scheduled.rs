use super::*;

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_manage_schedule_and_failures() -> Result<()> {
    // Task 52 retargeted this live test from the legacy `ScheduledTask`
    // document to the split `Task`/`Schedule` collections. The test
    // exercises the Schedule editor's round-trip (interval, enabled,
    // next_run_at, concurrency) plus the "recent failures" surfacing of
    // schedule errors. Task-side fields (name, description,
    // prompt_template) belong to the Task editor and are covered by the
    // in-repo unit tests; we keep the live test focused on the Schedule
    // surface so it doesn't need to navigate between the two detail
    // forms during a single flow.
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-scheduled", global_log_store())?;
    let docs = fixture.docs.clone();

    {
        let driver = &mut fixture.driver;
        driver.open_activity(Activity::Manage);
        driver.click_target(&audit::targets::manage_section(ManageSection::Schedules));
        driver.wait_for_target(
            "live schedule entity",
            Duration::from_secs(10),
            &audit::targets::manage_entity(&docs.schedule_id),
        )?;
        driver.click_target(&audit::targets::manage_entity(&docs.schedule_id));

        driver.scroll_right_rail_until_target(
            "live schedule interval field",
            &audit::targets::manage_field("Interval Secs"),
        )?;
        driver.replace_text_in_target(&audit::targets::manage_field("Interval Secs"), "0");
        driver.scroll_right_rail_until_target(
            "live schedule apply validation",
            audit::targets::MANAGE_APPLY,
        )?;
        driver.click_target(audit::targets::MANAGE_APPLY);
        assert!(driver
            .app
            .state
            .manage
            .last_apply_error
            .as_deref()
            .is_some_and(|error| error.contains("interval_secs must be greater than zero")));
        let validation_texts = wait_for_value(
            "live schedule validation error rendered",
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

        driver.replace_text_in_target(&audit::targets::manage_field("Interval Secs"), "120");
        driver.scroll_right_rail_until_target(
            "live schedule concurrency field",
            &audit::targets::manage_field("Concurrency"),
        )?;
        driver.replace_text_in_target(
            &audit::targets::manage_field("Concurrency"),
            "latest_only",
        );
        driver.scroll_right_rail_until_target(
            "live schedule enabled toggle",
            &audit::targets::manage_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::manage_toggle("Enabled"));
        // `next_run_at` is runtime-owned (Task 53): the desktop shows
        // it as read-only and the mutation writer never projects it.
        // We don't try to edit it from the UI here.
        driver.scroll_right_rail_until_target(
            "live schedule apply",
            audit::targets::MANAGE_APPLY,
        )?;
        driver.click_target(audit::targets::MANAGE_APPLY);
        wait_for_value(
            "live schedule edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .schedules
                        .iter()
                        .find(|row| row.schedule_id == docs.schedule_id)
                        .filter(|row| {
                            row.interval_secs == Some(120)
                                && row.concurrency.as_deref() == Some("latest_only")
                                && row.enabled == Some(false)
                        })
                        .map(|row| row.schedule_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::manage_entity(&docs.schedule_id));
        driver.scroll_right_rail_until_target(
            "live schedule re-enable toggle",
            &audit::targets::manage_toggle("Enabled"),
        )?;
        driver.click_target(&audit::targets::manage_toggle("Enabled"));
        driver.scroll_right_rail_until_target(
            "live schedule re-enable apply",
            audit::targets::MANAGE_APPLY,
        )?;
        driver.click_target(audit::targets::MANAGE_APPLY);
        wait_for_value(
            "live schedule re-enabled",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .schedules
                        .iter()
                        .find(|row| row.schedule_id == docs.schedule_id)
                        .filter(|row| row.enabled == Some(true))
                        .map(|row| row.schedule_id.clone())
                })
            },
        )?;

        // The manual "run now" path is intentionally stubbed in PR 1;
        // PR 3 will wire the real manual-run surface. Assert the stub
        // behavior here so a regression in the wiring is caught.
        driver.click_target(&audit::targets::manage_entity(&docs.schedule_id));
        driver.scroll_right_rail_until_target(
            "live schedule run-now button",
            audit::targets::MANAGE_RUN_NOW,
        )?;
        driver.click_target(audit::targets::MANAGE_RUN_NOW);
        assert!(
            driver
                .app
                .state
                .manage
                .last_apply_error
                .as_deref()
                .is_some_and(|error| error.contains("PR 3")),
            "expected a PR 3 stub error on run-now; got {:?}",
            driver.app.state.manage.last_apply_error,
        );

        // Inject a Schedule failure directly through the writer so the
        // Recent Failures view has something to surface. The writer
        // normally ignores runtime-owned fields, but we construct the
        // row directly and then patch it via a raw GraphQL mutation to
        // simulate what the scheduler would write.
        let existing = driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .schedules
                    .iter()
                    .find(|row| row.schedule_id == docs.schedule_id)
                    .cloned()
            })
            .ok_or_else(|| anyhow!("missing live schedule before failure insert"))?;
        let mut failed = existing;
        failed.last_status = Some("error".to_string());
        failed.last_error = Some("live scheduled audit failure".to_string());
        failed.last_attempt_at = Some(chrono::Utc::now().to_rfc3339());
        // `save_schedule` only projects apply-owned fields. We need to
        // write the runtime-owned bookkeeping fields via a raw
        // mutation to stage the failure for the Recent Failures view.
        let client = Arc::clone(
            driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        let last_status = failed.last_status.clone().unwrap_or_default();
        let last_error = failed.last_error.clone().unwrap_or_default();
        let last_attempt_at = failed.last_attempt_at.clone().unwrap_or_default();
        let schedule_id_for_filter = docs.schedule_id.clone();
        driver.app.runtime.block_on(async {
            let resp = client
                .node()
                .execute(&format!(
                    r#"mutation {{
                        update_Schedule(
                            filter: {{ schedule_id: {{ _eq: "{schedule_id}" }} }},
                            input: {{
                                last_status: "{last_status}"
                                last_error: "{last_error}"
                                last_attempt_at: "{last_attempt_at}"
                            }}
                        ) {{ _docID }}
                    }}"#,
                    schedule_id = escape_graphql_string(&schedule_id_for_filter),
                    last_status = escape_graphql_string(&last_status),
                    last_error = escape_graphql_string(&last_error),
                    last_attempt_at = escape_graphql_string(&last_attempt_at),
                ))
                .await;
            if resp.has_errors() {
                anyhow::bail!("update_Schedule (failure bookkeeping) failed: {:?}", resp.errors);
            }
            client.refresh_store().await?;
            Ok::<_, anyhow::Error>(())
        })?;
        wait_for_value(
            "live schedule failure persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .schedules
                        .iter()
                        .find(|row| row.schedule_id == docs.schedule_id)
                        .filter(|row| {
                            row.last_status.as_deref() == Some("error")
                                && row.last_error.as_deref() == Some("live scheduled audit failure")
                        })
                        .map(|row| row.schedule_id.clone())
                })
            },
        )?;

        driver.app.state.manage.selected_entity_id = None;
        driver.app.state.manage.draft = None;
        driver.app.state.manage.draft_origin = None;
        driver.click_target(&audit::targets::manage_section(
            crate::state::ManageSection::RecentFailures,
        ));
        let failure_id = format!("schedule:{}", docs.schedule_id);
        driver.wait_for_target(
            "live schedule failure row",
            Duration::from_secs(10),
            &audit::targets::manage_entity(&failure_id),
        )?;
        let failure_texts = driver.click_target(&audit::targets::manage_entity(&failure_id));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("Failure Detail")));
        assert!(failure_texts
            .iter()
            .any(|text| text.contains("live scheduled audit failure")));
    }

    fixture.shutdown()
}
