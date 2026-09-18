//! Independent acceptance checks for progressively generated behaviors.

use super::readiness::recorded_command as recorded_readiness_command;
use super::{exact_named_behavior, rows, stages};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;

/// Selected configuration only, excluding timestamps/runtime observations.
/// Check preservation independently of the configurator's own receipts.
async fn behavior_configuration(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    behavior_id: Option<&str>,
) -> Result<serde_json::Value> {
    let owner = gents::graphql::escape_graphql_string(owner);
    let behaviors = rows(node, &format!(r#"{{ AgentBehavior(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{behavior_id display_name context_id inference_profile_id tags enabled}} }}"#), "AgentBehavior").await?;
    let behavior = match behavior_id {
        Some(id) => behaviors
            .iter()
            .find(|row| row["behavior_id"] == id)
            .context("selected behavior missing")?,
        None => exact_named_behavior(&behaviors, "Builder")?,
    };
    let contexts = rows(node, &format!(r#"{{ AgentContext(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{context_id tools_id system_prompt skill_ids compaction_id}} }}"#), "AgentContext").await?;
    let context = contexts
        .iter()
        .find(|row| row["context_id"] == behavior["context_id"])
        .context("behavior context missing")?;
    let tools = rows(node, &format!(r#"{{ Tools(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{tools_id host built_ins integrations remote subagents datastore self_config}} }}"#), "Tools").await?;
    let tools = tools
        .iter()
        .find(|row| row["tools_id"] == context["tools_id"])
        .context("behavior Tools missing")?;
    Ok(serde_json::json!({"behavior":behavior,"context":context,"tools":tools}))
}

/// Exercise the generated default in a new session without changing its grants.
pub(super) async fn verify_builder_execution(
    activation: &stages::ActivationFence,
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    user_home: &str,
    evidence: &std::path::Path,
) -> Result<()> {
    let owner = gents::graphql::escape_graphql_string(agent_did);
    let principals = rows(
        node,
        &format!(r#"{{ AgentPrincipal(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{default_behavior_id}} }}"#),
        "AgentPrincipal",
    ).await?;
    let builder = principals
        .first()
        .and_then(|row| row["default_behavior_id"].as_str())
        .context("generated default behavior is missing")?;
    let result = stages::execute(
        activation,
        node,
        agent_did,
        builder,
        "builder-readiness",
        &include_str!("../fixtures/configurator_evals/builder_readiness.md")
            .trim_end()
            .replace("{{USER_HOME}}", user_home),
        evidence,
    )
    .await?;
    result.ensure_completed()?;
    let receipt =
        std::fs::read_to_string(std::path::Path::new(user_home).join("readiness/result.txt"))
            .context("Builder did not produce the build/test receipt")?;
    ensure!(
        receipt == "BUILD_TEST_OK\n",
        "Builder build/test receipt is incorrect"
    );
    let script =
        std::fs::read_to_string(std::path::Path::new(user_home).join("readiness/test.sh"))?;
    ensure!(script == "#!/bin/sh\nset -eu\ntest \"$((2 + 2))\" -eq 4\nprintf 'BUILD_TEST_OK\\n' > \"$(dirname \"$0\")/result.txt\"\n",
        "Builder changed the requested test script");
    let calls = rows(
        node,
        &format!(
            r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}}}) {{tool_name lifecycle_state args result}} }}"#,
            gents::graphql::escape_graphql_string(&result.request_id)
        ),
        "AgentToolCall",
    )
    .await?;
    if !calls
        .iter()
        .any(|call| recorded_readiness_command(call, std::path::Path::new(user_home)))
    {
        return Err(stages::EvaluationFailure::Inconclusive(
            "command evidence did not identify execution of the readiness script".into(),
        )
        .into());
    }
    Ok(())
}

#[test]
fn readiness_command_evidence_accounts_for_cwd() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("readiness")).unwrap();
    std::fs::write(root.path().join("readiness/test.sh"), "# fixture").unwrap();
    let call = |args: serde_json::Value, state: &str| {
        serde_json::json!({
            "tool_name":"bash_unrestricted", "lifecycle_state":state, "args":args.to_string(),
            "result":"gents_exec: {\"ok\":true,\"exit_code\":0}"
        })
    };
    for result in [
        "",
        "BUILD_TEST_OK",
        "gents_exec: {\"ok\":false,\"exit_code\":1}",
        "gents_exec: {\"ok\":true,\"exit_code\":0,\"timed_out\":true}",
    ] {
        let mut evidence = call(
            serde_json::json!({"command":"sh readiness/test.sh"}),
            "completed",
        );
        evidence["result"] = result.into();
        assert!(!recorded_readiness_command(&evidence, root.path()));
    }
    for args in [
        serde_json::json!({"command":"sh readiness/test.sh"}),
        serde_json::json!({"command":"sh readiness/test.sh; # executed"}),
        serde_json::json!({"command":"sh 'readiness/test.sh'","args":[]}),
        serde_json::json!({"command":"sh","args":["readiness/test.sh"]}),
        serde_json::json!({"command":"/bin/sh","args":[root.path().join("readiness/test.sh")],"cwd":"."}),
        serde_json::json!({"command":"sh test.sh","cwd":"readiness"}),
        serde_json::json!({"command":"sh","args":["./test.sh"],"cwd":root.path().join("readiness")}),
        serde_json::json!({"command":"sh readiness/test.sh && echo \"exit=$?\""}),
        serde_json::json!({"command":"sh", "args":["-lc", "mkdir -p readiness && cat > readiness/test.sh <<'EOF'\nsh readiness/test.sh\nEOF\nchmod +x readiness/test.sh && sh readiness/test.sh && cat readiness/result.txt"]}),
    ] {
        assert!(recorded_readiness_command(
            &call(args.clone(), "completed"),
            root.path()
        ));
        assert!(!recorded_readiness_command(
            &call(args, "failed"),
            root.path()
        ));
    }
    for args in [
        serde_json::json!({"command":"sh test.sh","cwd":"."}),
        serde_json::json!({"command":"echo test.sh","cwd":"readiness"}),
        serde_json::json!({"command":"true","note":"readiness/test.sh"}),
        serde_json::json!({"command":"echo readiness/test.sh"}),
        serde_json::json!({"command":"sh readiness/test.sh.backup"}),
        serde_json::json!({"command":"sh -n readiness/test.sh"}),
        serde_json::json!({"command":"true && echo readiness/test.sh"}),
        serde_json::json!({"command":"sh $SCRIPT"}),
        serde_json::json!({"command":"sh readiness/tes?.sh"}),
        serde_json::json!({"command":"sh\nreadiness/test.sh"}),
        serde_json::json!({"command":"sh 'readiness/test.sh"}),
        serde_json::json!({"command":"sh readiness/test.sh","args":["ignored"]}),
        serde_json::json!({"command":"sh","args":["unrelated/test.sh"]}),
        serde_json::json!({"command":"sh","args":["sh","readiness/test.sh"]}),
        serde_json::json!({"command":"true || sh readiness/test.sh"}),
        serde_json::json!({"command":"false && sh readiness/test.sh; true"}),
        serde_json::json!({"command":"if false; then sh readiness/test.sh; fi"}),
        serde_json::json!({"command":"echo 'sh readiness/test.sh && echo ok'"}),
        serde_json::json!({"command":"cat <<'EOF'\nsh readiness/test.sh\nEOF"}),
        serde_json::json!({"command":"f() { sh readiness/test.sh; }; true"}),
        serde_json::json!({"command":"cd elsewhere; sh readiness/test.sh"}),
        serde_json::json!({"command":"sh readiness/test.sh &"}),
        serde_json::json!({"command":"echo $(sh readiness/test.sh)"}),
        serde_json::json!({"command":"mkdir /unwritable && sh readiness/test.sh; true"}),
        serde_json::json!({"command":"mkdir /unwritable && sh readiness/test.sh\ntrue"}),
    ] {
        assert!(!recorded_readiness_command(
            &call(args, "completed"),
            root.path()
        ));
    }
}

fn reassess_readiness(
    original: &stages::CaseResult,
    calls: &[serde_json::Value],
    workspace: &std::path::Path,
) -> Option<stages::CaseResult> {
    // Correct only the known attribution bug. That check ran after request
    // completion and exact script/receipt validation; other failures stand.
    if original.case_id != "builder-readiness"
        || original.status != "failed"
        || !matches!(
            original.error.as_deref(),
            Some(
                "Builder did not successfully execute its command tool"
                    | "Builder command evidence did not identify the readiness script"
                    | "evaluation inconclusive: command evidence did not identify execution of the readiness script"
            )
        )
        || !calls
            .iter()
            .any(|call| recorded_readiness_command(call, workspace))
    {
        return None;
    }
    Some(stages::CaseResult {
        case_id: original.case_id.clone(),
        status: "passed".into(),
        elapsed_ms: original.elapsed_ms,
        error: None,
        failure_kind: None,
    })
}

#[test]
fn readiness_reassessment_does_not_hide_other_failures() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("readiness")).unwrap();
    std::fs::write(root.path().join("readiness/test.sh"), "# fixture").unwrap();
    let calls = vec![serde_json::json!({"tool_name":"bash_unrestricted",
        "lifecycle_state":"completed", "result":"gents_exec: {\"ok\":true,\"exit_code\":0}", "args":serde_json::json!({"command":"sh test.sh","cwd":"readiness"}).to_string()})];
    let mut original = stages::CaseResult {
        case_id: "builder-readiness".into(),
        status: "failed".into(),
        elapsed_ms: 123,
        error: Some("Builder did not successfully execute its command tool".into()),
        failure_kind: Some("acceptance".into()),
    };
    let corrected = reassess_readiness(&original, &calls, root.path()).unwrap();
    assert_eq!(corrected.status, "passed");
    assert_eq!(corrected.elapsed_ms, 123);
    assert!(reassess_readiness(&original, &[], root.path()).is_none());
    original.error = Some("Builder command evidence did not identify the readiness script".into());
    let structured = vec![serde_json::json!({"tool_name":"bash_unrestricted",
        "lifecycle_state":"completed", "result":"gents_exec: {\"ok\":true,\"exit_code\":0}", "args":serde_json::json!({"command":"sh","args":["readiness/test.sh"]}).to_string()})];
    assert_eq!(
        reassess_readiness(&original, &structured, root.path())
            .unwrap()
            .status,
        "passed"
    );
    original.error = Some("Builder changed the requested test script".into());
    assert!(reassess_readiness(&original, &calls, root.path()).is_none());
    assert!(reassess_readiness(&original, &structured, root.path()).is_none());
}

/// Offline grading never invokes inference or overwrites the original report.
fn retained_readiness_result(trial: &std::path::Path) -> Result<Option<stages::CaseResult>> {
    match std::fs::read(trial.join("evidence/builder-readiness-acceptance.json")) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(trial.join("trial.json"))?)?;
            ensure!(
                report["cases"].as_array().is_some_and(|cases| cases
                    .iter()
                    .any(|case| case["case_id"] == "builder-readiness"
                        && case["status"] == "skipped")),
                "readiness receipt missing without an explicit skipped case"
            );
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[test]
fn reassessment_distinguishes_skipped_readiness_from_missing_evidence() {
    let trial = tempfile::tempdir().unwrap();
    assert!(retained_readiness_result(trial.path()).is_err());
    std::fs::write(
        trial.path().join("trial.json"),
        r#"{"cases":[{"case_id":"builder-readiness","status":"skipped"}]}"#,
    )
    .unwrap();
    assert!(retained_readiness_result(trial.path()).unwrap().is_none());
    std::fs::write(
        trial.path().join("trial.json"),
        r#"{"cases":[{"case_id":"builder-readiness","status":"passed"}]}"#,
    )
    .unwrap();
    assert!(retained_readiness_result(trial.path()).is_err());
}

#[test]
#[ignore = "offline: set GENTS_EVAL_REASSESS_TRIALS to a platform-separated list of retained trial directories"]
fn reassess_retained_readiness_evidence() -> Result<()> {
    let paths = std::env::var_os("GENTS_EVAL_REASSESS_TRIALS")
        .context("set GENTS_EVAL_REASSESS_TRIALS to retained trial directories")?;
    for trial in std::env::split_paths(&paths) {
        let evidence = trial.join("evidence");
        let Some(original) = retained_readiness_result(&trial)? else {
            tracing::info!(trial = %trial.display(), "readiness was skipped; no reassessment");
            continue;
        };
        let tool_evidence: serde_json::Value = serde_json::from_slice(&std::fs::read(
            evidence.join("builder-readiness-tools.json"),
        )?)?;
        let calls = tool_evidence["AgentToolCall"]
            .as_array()
            .context("missing retained tool calls")?;
        let reassessed = reassess_readiness(&original, calls, &trial.join("workspace"));
        std::fs::write(
            evidence.join("builder-readiness-reassessment-shell-ast-v4.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "grader":"readiness-shell-ast-v4", "changed":reassessed.is_some(),
                "original":original, "reassessed":reassessed.as_ref().unwrap_or(&original),
                "basis":"Retained request tool calls; original completion, exact script and receipt checks remain required. No inference rerun."
            }))?,
        )?;
    }
    Ok(())
}

fn pagoda_prompt(template: &str, workspace: &std::path::Path) -> String {
    template
        .replace(
            "{{ORIGINAL_REQUEST}}",
            include_str!("../fixtures/configurator_evals/voxel_pagoda.md"),
        )
        .replace("{{USER_HOME}}", &workspace.to_string_lossy())
}

#[test]
fn fresh_pagoda_sessions_receive_the_complete_original_request() {
    let workspace = std::path::Path::new("/isolated/workspace");
    let original = pagoda_prompt(
        include_str!("../fixtures/configurator_evals/voxel_pagoda.md"),
        workspace,
    );
    for template in [
        include_str!("../fixtures/configurator_evals/review_pagoda.md"),
        include_str!("../fixtures/configurator_evals/improve_pagoda.md"),
    ] {
        let rendered = pagoda_prompt(template, workspace);
        assert!(rendered.contains(&original));
        assert!(rendered.contains("README.md"));
        assert!(rendered.contains("HTML document title"));
        assert!(rendered.contains("symlinks"));
        assert!(!rendered.contains("{{ORIGINAL_REQUEST}}"));
        assert!(!rendered.contains("{{USER_HOME}}"));
    }
}

pub(super) async fn verify_pagoda_sequence(
    activation: &stages::ActivationFence,
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    workspace: &std::path::Path,
    evidence: &std::path::Path,
) -> Result<()> {
    let escaped = gents::graphql::escape_graphql_string(owner);
    let behaviors = rows(node, &format!(
        r#"{{ AgentBehavior(filter: {{agent_did: {{_eq: "{escaped}"}}}}) {{behavior_id display_name enabled}} }}"#
    ), "AgentBehavior").await?;
    let builder = exact_named_behavior(&behaviors, "Builder")?["behavior_id"]
        .as_str()
        .context("Builder identity")?;
    let reviewer = exact_named_behavior(&behaviors, "Reviewer")?["behavior_id"]
        .as_str()
        .context("Reviewer identity")?;
    let project = workspace.join("pagoda");
    let prompt = |text: &str| pagoda_prompt(text, workspace);
    let creation = stages::checked(
        stages::CaseId::Pagoda,
        evidence,
        retain_checked_project(&project, &evidence.join("pagoda-source"), async {
            let created = stages::execute(
                activation,
                node,
                owner,
                builder,
                "pagoda",
                &prompt(include_str!(
                    "../fixtures/configurator_evals/voxel_pagoda.md"
                )),
                evidence,
            )
            .await?;
            created.ensure_completed()?;
            check_pagoda_browser(&project, &evidence.join("pagoda-browser")).await?;
            Ok(())
        }),
    )
    .await;
    // A failed first attempt can still be reviewed and improved. Retain its
    // failure, and require actual model-authored HTML rather than seeding a
    // substitute artifact to make the later cases runnable.
    if !std::fs::metadata(project.join("index.html"))
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    {
        return creation.and(Err(anyhow::anyhow!(
            "no HTML artifact available for review"
        )));
    }
    let review = stages::checked(stages::CaseId::Review, evidence, async {
        let before = project_snapshot(workspace)?;
        let review = stages::execute(
            activation,
            node,
            owner,
            reviewer,
            "review",
            &prompt(include_str!(
                "../fixtures/configurator_evals/review_pagoda.md"
            )),
            evidence,
        )
        .await?;
        review.ensure_completed()?;
        ensure!(
            before == project_snapshot(workspace)?,
            "Reviewer modified workspace files"
        );
        ensure!(
            !review.answer.trim().is_empty(),
            "Reviewer returned no feedback"
        );
        Ok(review)
    })
    .await;
    let review = match review {
        Ok(review) => review,
        Err(error) => return combine_case_outcomes(creation, Err(error)),
    };
    let improvement = stages::checked(
        stages::CaseId::Improve,
        evidence,
        retain_checked_project(&project, &evidence.join("improve-source"), async {
            let improved = stages::execute(
                activation,
                node,
                owner,
                builder,
                "improve",
                &prompt(include_str!(
                    "../fixtures/configurator_evals/improve_pagoda.md"
                ))
                .replace("{{REVIEW}}", &review.answer),
                evidence,
            )
            .await?;
            improved.ensure_completed()?;
            check_pagoda_browser(&project, &evidence.join("improve-browser")).await
        }),
    )
    .await;
    combine_case_outcomes(creation, improvement)
}

fn combine_case_outcomes(first: Result<()>, second: Result<()>) -> Result<()> {
    match (first, second) {
        (Err(first), Err(second)) => {
            Err(first.context(format!("later case also failed: {second:#}")))
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[test]
fn sequence_summary_retains_both_failures() {
    let error = combine_case_outcomes(
        Err(stages::EvaluationFailure::Deadline("pagoda".into()).into()),
        Err(stages::EvaluationFailure::ModelRequest("review".into()).into()),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("pagoda"));
    assert!(message.contains("review"));
}

pub(super) async fn verify_skill_workflow(
    activation: &stages::ActivationFence,
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    setup: &str,
    workspace: &std::path::Path,
    evidence: &std::path::Path,
) -> Result<()> {
    let setup_before = behavior_configuration(node, owner, Some(setup)).await?;
    let mut builder_before = behavior_configuration(node, owner, None).await?;
    let source = workspace.join("skill-fixture");
    std::fs::create_dir_all(source.join("references"))?;
    std::fs::write(source.join("SKILL.md"),
        "---\nname: coding-check\ndescription: Run the configured coding readiness procedure\n---\nFor the configured readiness procedure, read references/checklist.md relative to this skill directory and carry out its instructions.\n")?;
    let marker = format!("SKILL_CHECK_{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(workspace.join("references"))?;
    std::fs::write(workspace.join("references/checklist.md"),
        "This is not the skill's checklist. Do not create a receipt from this file; resolve the reference from the skill's source directory.\n")?;
    std::fs::write(source.join("references/checklist.md"),
        format!("In your working root, write readiness/skill-check.txt containing exactly {marker} followed by a newline. Read it back and report the result.\n"))?;
    let configured = stages::execute(
        activation,
        node,
        owner,
        setup,
        "skill-setup",
        &include_str!("../fixtures/configurator_evals/skill_setup.md")
            .trim_end()
            .replace("{{SKILL_DIRECTORY}}", &source.to_string_lossy()),
        evidence,
    )
    .await?;
    configured.ensure_completed()?;
    activation.wait().await?;
    let imported = rows(node, &format!(
        r#"{{ Skill(filter: {{agent_did: {{_eq: "{}"}}, skill_id: {{_eq: "eval-coding-check"}}}}) {{source_directory}} }}"#,
        gents::graphql::escape_graphql_string(owner)
    ), "Skill").await?;
    ensure!(
        imported.len() == 1
            && imported[0]["source_directory"].as_str() == source.canonicalize()?.to_str(),
        "imported skill did not preserve its canonical source directory"
    );
    ensure!(
        setup_before == behavior_configuration(node, owner, Some(setup)).await?,
        "skill import changed Setup"
    );
    let mut builder_after = behavior_configuration(node, owner, None).await?;
    let skills = builder_after["context"]["skill_ids"]
        .as_array()
        .context("Builder has no skills")?;
    ensure!(
        skills.iter().any(|id| id == "eval-coding-check"),
        "imported skill is not attached to Builder"
    );
    if let Some(previous) = builder_before["context"]["skill_ids"].as_array() {
        ensure!(
            previous.iter().all(|id| skills.contains(id)),
            "skill import removed an existing attachment"
        );
    }
    builder_before["context"]["skill_ids"] = serde_json::Value::Null;
    builder_after["context"]["skill_ids"] = serde_json::Value::Null;
    ensure!(
        builder_before == builder_after,
        "skill import changed unrelated Builder configuration"
    );
    let escaped = gents::graphql::escape_graphql_string(owner);
    let behaviors = rows(node, &format!(
        r#"{{ AgentBehavior(filter: {{agent_did: {{_eq: "{escaped}"}}}}) {{behavior_id display_name enabled}} }}"#
    ), "AgentBehavior").await?;
    let builder = exact_named_behavior(&behaviors, "Builder")?["behavior_id"]
        .as_str()
        .context("Builder identity")?;
    ensure!(
        !workspace.join("readiness/skill-check.txt").exists(),
        "Setup executed the skill instead of configuring Builder"
    );
    let executed = stages::execute(
        activation,
        node,
        owner,
        builder,
        "skill-use",
        include_str!("../fixtures/configurator_evals/skill_use.md").trim_end(),
        evidence,
    )
    .await?;
    executed.ensure_completed()?;
    ensure!(
        std::fs::read_to_string(workspace.join("readiness/skill-check.txt"))?
            == format!("{marker}\n"),
        "skill supporting-file procedure produced the wrong receipt"
    );
    let calls = rows(
        node,
        &format!(
            r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}}}) {{tool_name lifecycle_state}} }}"#,
            gents::graphql::escape_graphql_string(&executed.request_id)
        ),
        "AgentToolCall",
    )
    .await?;
    ensure!(
        calls
            .iter()
            .any(|call| call["tool_name"] == "load_skill" && call["lifecycle_state"] == "completed"),
        "Builder did not successfully load its skill"
    );
    Ok(())
}

pub(super) async fn verify_document_automation(
    activation: &stages::ActivationFence,
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    setup: &str,
    evidence: &std::path::Path,
) -> Result<()> {
    let result = run_document_automation(activation, node, owner, setup, evidence).await;
    let retention = async {
    // Persist dispatch failures too: a bad template can fail before there is
    // any AgentRequest or inference call to inspect.
    let escaped = gents::graphql::escape_graphql_string(owner);
    let diagnostics = node.execute(&format!(r#"{{
        Trigger(filter: {{agent_did: {{_eq: "{escaped}"}}}}) {{trigger_id task_id last_error}}
        Task(filter: {{agent_did: {{_eq: "{escaped}"}}}}) {{task_id behavior_id prompt_template}}
        AgentRequest(filter: {{agent_did: {{_eq: "{escaped}"}}}}) {{request_id caused_by_trigger_id lifecycle_state}}
    }}"#)).await;
    std::fs::write(
        evidence.join("automation-dispatch.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"data":diagnostics.data,"errors":format!("{:?}", diagnostics.errors)}),
        )?,
    )?;
    let trigger_ids = automation_trigger_ids(node, owner).await?;
    if let Some(requests) = diagnostics
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
    {
        for (index, request) in requests.iter().enumerate() {
            // Names are not ownership evidence: the model may legitimately
            // choose an eval-trigger-* ID for its own workflow too.
            if trigger_ids.contains(&request["caused_by_trigger_id"]) {
                if let Some(id) = request["request_id"].as_str() {
                    stages::retain_request_evidence(
                        node,
                        id,
                        &format!("automation-request-{index}"),
                        evidence,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
    }.await;
    stages::retain_outcome(result, retention)
}

async fn automation_trigger_ids(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
) -> Result<Vec<serde_json::Value>> {
    let escaped_owner = gents::graphql::escape_graphql_string(owner);
    let sources = rows(node, &format!(r#"{{ EventSource(filter: {{agent_did: {{_eq: "{escaped_owner}"}}, source_collection: {{_eq: "EvalAutomationInput"}}}}) {{event_source_id}} }}"#), "EventSource").await?;
    let triggers = rows(node, &format!(r#"{{ Trigger(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{trigger_id source}} }}"#), "Trigger").await?;
    Ok(triggers
        .iter()
        .filter(|trigger| {
            sources
                .iter()
                .any(|source| trigger["source"]["event_source_id"] == source["event_source_id"])
        })
        .map(|trigger| trigger["trigger_id"].clone())
        .collect())
}

async fn run_document_automation(
    activation: &stages::ActivationFence,
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    setup: &str,
    evidence: &std::path::Path,
) -> Result<()> {
    let setup_before = behavior_configuration(node, owner, Some(setup)).await?;
    let mut builder_before = behavior_configuration(node, owner, None).await?;
    let configured = stages::execute(
        activation,
        node,
        owner,
        setup,
        "automation-setup",
        include_str!("../fixtures/configurator_evals/document_automation.md"),
        evidence,
    )
    .await?;
    configured.ensure_completed()?;
    activation.wait().await?;
    ensure!(
        setup_before == behavior_configuration(node, owner, Some(setup)).await?,
        "automation changed Setup"
    );
    let mut builder_after = behavior_configuration(node, owner, None).await?;
    let datastore = &builder_after["tools"]["datastore"];
    ensure!(
        datastore["enable_defra_query"] != true,
        "automation enabled unrestricted query tools"
    );
    ensure!(
        datastore["datastore_tool_surface_ids"]
            .as_array()
            .is_some_and(|ids| !ids.is_empty()),
        "automation did not select a narrow datastore surface"
    );
    builder_before["tools"]["datastore"] = serde_json::Value::Null;
    builder_after["tools"]["datastore"] = serde_json::Value::Null;
    ensure!(
        builder_before == builder_after,
        "automation changed unrelated Builder configuration"
    );
    let before = rows(
        node,
        "{ EvalAutomationOutput { correlation result } }",
        "EvalAutomationOutput",
    )
    .await?;
    ensure!(
        before.is_empty(),
        "automation output was precreated before any input"
    );
    let mut receipts = Vec::new();
    for message in ["Hello pagoda", "the garden is green"] {
        let correlation = uuid::Uuid::new_v4().to_string();
        let escaped_correlation = gents::graphql::escape_graphql_string(&correlation);
        let escaped_message = gents::graphql::escape_graphql_string(message);
        // This external-client input write is the only invocation. The model
        // must have authored all schemas, tools, tasks, and trigger bindings.
        let response = node.execute(&format!(
            r#"mutation {{ create_EvalAutomationInput(input: {{correlation: "{escaped_correlation}", message: "{escaped_message}"}}) {{_docID}} }}"#
        )).await;
        ensure!(
            !response.has_errors(),
            "automation input submission failed: {:?}",
            response.errors
        );
        receipts.push(serde_json::json!({
            "input": message, "correlation": correlation,
            "submission": response.data, "output": null,
        }));
        std::fs::write(
            evidence.join("automation-receipts.json"),
            serde_json::to_vec_pretty(&receipts)?,
        )?;
        let started = std::time::Instant::now();
        let output = loop {
            let output = rows(node, &format!(
                r#"{{ EvalAutomationOutput(filter: {{correlation: {{_eq: "{escaped_correlation}"}}}}) {{correlation result}} }}"#
            ), "EvalAutomationOutput").await?;
            if !output.is_empty() {
                break output;
            }
            ensure!(
                started.elapsed() < std::time::Duration::from_secs(120),
                "document-triggered automation produced no output for {correlation}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        };
        receipts.last_mut().context("automation input receipt")?["output"] =
            serde_json::json!(output);
        std::fs::write(
            evidence.join("automation-receipts.json"),
            serde_json::to_vec_pretty(&receipts)?,
        )?;
        ensure!(
            output.len() == 1 && output[0]["result"] == message.to_uppercase(),
            "automation output is duplicated or incorrect: {output:?}"
        );
    }
    let escaped_owner = gents::graphql::escape_graphql_string(owner);
    let trigger_ids = automation_trigger_ids(node, owner).await?;
    ensure!(
        !trigger_ids.is_empty(),
        "no trigger watches the input collection"
    );
    let started = std::time::Instant::now();
    loop {
        let requests = rows(node, &format!(r#"{{ AgentRequest(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{request_id caused_by_trigger_id lifecycle_state}} }}"#), "AgentRequest").await?;
        let requests = requests
            .iter()
            .filter(|request| trigger_ids.contains(&request["caused_by_trigger_id"]))
            .collect::<Vec<_>>();
        if requests.len() >= receipts.len()
            && requests.iter().all(|request| {
                gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
                    request["lifecycle_state"].as_str(),
                )
            })
        {
            break;
        }
        ensure!(
            started.elapsed() < std::time::Duration::from_secs(120),
            "automation requests did not terminalize: {requests:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    // Recheck the complete result set after both inputs. A late duplicate from
    // the first delivery must not escape the per-input first-result check.
    let outputs = rows(
        node,
        "{ EvalAutomationOutput { correlation result } }",
        "EvalAutomationOutput",
    )
    .await?;
    ensure!(
        outputs.len() == receipts.len(),
        "automation created unexpected or duplicate outputs: {outputs:?}"
    );
    for receipt in &receipts {
        let matches = outputs
            .iter()
            .filter(|row| row["correlation"] == receipt["correlation"])
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1
                && matches[0]["result"]
                    == receipt["input"]
                        .as_str()
                        .context("input message")?
                        .to_uppercase(),
            "automation final outputs do not match inputs: {outputs:?}"
        );
    }
    Ok(())
}

fn retain_project(
    root: &std::path::Path,
    evidence: &std::path::Path,
    snapshot: &BTreeMap<std::path::PathBuf, String>,
) -> Result<()> {
    for path in snapshot.keys() {
        let target = evidence.join(path);
        std::fs::create_dir_all(target.parent().context("evidence parent")?)?;
        std::fs::copy(root.join(path), target)?;
    }
    std::fs::write(
        evidence.join("checksums.json"),
        serde_json::to_vec_pretty(snapshot)?,
    )?;
    Ok(())
}

async fn retain_checked_project(
    project: &std::path::Path,
    evidence: &std::path::Path,
    check: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    let outcome = check.await;
    let retained =
        project_snapshot(project).and_then(|snapshot| retain_project(project, evidence, &snapshot));
    match (outcome, retained) {
        (Err(outcome), Err(retention)) => {
            Err(outcome.context(format!("artifact retention also failed: {retention:#}")))
        }
        (Err(outcome), Ok(())) => Err(outcome),
        (Ok(()), retained) => retained,
    }
}

#[tokio::test]
async fn failed_project_checks_retain_source_without_losing_the_failure_kind() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("index.html"), "partial model output").unwrap();
    for source in [&project, &root.path().join("missing-project")] {
        let error = retain_checked_project(source, &root.path().join("evidence"), async {
            Err(stages::EvaluationFailure::Deadline("improve".into()).into())
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<stages::EvaluationFailure>(),
            Some(stages::EvaluationFailure::Deadline(_))
        ));
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("evidence/index.html")).unwrap(),
        "partial model output"
    );
}

fn project_snapshot(root: &std::path::Path) -> Result<BTreeMap<std::path::PathBuf, String>> {
    use sha2::{Digest, Sha256};
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        ensure!(
            snapshot.len() + pending.len() < 10_000,
            "project exceeds evaluator file budget"
        );
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "project must not contain symlinks"
        );
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else {
            ensure!(
                metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
                "project contains a nonregular or oversized file"
            );
            snapshot.insert(
                path.strip_prefix(root)?.to_owned(),
                format!("{:x}", Sha256::digest(std::fs::read(&path)?)),
            );
        }
    }
    Ok(snapshot)
}

async fn check_pagoda_browser(project: &std::path::Path, evidence: &std::path::Path) -> Result<()> {
    ensure!(
        project.join("README.md").is_file(),
        "pagoda README.md is missing"
    );
    // Validate filesystem bounds before opening the model-authored page.
    project_snapshot(project)?;
    // Freeze evaluator code with the compiled fixtures for the entire batch.
    // Working-tree edits during a long run must not change later trials' checks.
    let script = include_str!("../../../../scripts/evals/check-pagoda.mjs");
    std::fs::create_dir_all(evidence)?;
    std::fs::write(evidence.join("checker.mjs"), script)?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new("node")
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .args([
                "--input-type=module",
                "--eval",
                script,
                "--",
                "check-pagoda.mjs",
            ])
            .arg(project)
            .arg(evidence)
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) => output,
        failure => {
            let reason = match failure {
                Err(_) => "browser evaluator exceeded its 60-second deadline".to_owned(),
                Ok(Err(error)) => format!("launching browser evaluator: {error}"),
                Ok(Ok(_)) => unreachable!(),
            };
            std::fs::write(
                evidence.join("process.json"),
                serde_json::to_vec_pretty(&serde_json::json!({"infrastructure_error":reason}))?,
            )?;
            return Err(stages::EvaluationFailure::Infrastructure(reason).into());
        }
    };
    std::fs::write(
        evidence.join("process.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))?,
    )?;
    let receipt = read_browser_receipt(evidence, &String::from_utf8_lossy(&output.stderr))?;
    if receipt["inconclusive"] == true {
        return Err(stages::EvaluationFailure::Inconclusive(receipt["error"].to_string()).into());
    }
    ensure!(
        output.status.success(),
        "browser acceptance failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn read_browser_receipt(evidence: &std::path::Path, stderr: &str) -> Result<serde_json::Value> {
    let decoded = std::fs::read(evidence.join("browser.json"))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(Into::into));
    let receipt = decoded.map_err(|error| {
        stages::EvaluationFailure::Infrastructure(format!(
            "browser evaluator did not produce a readable receipt: {error}; stderr: {stderr}"
        ))
    })?;
    if !receipt["passed"].is_boolean() {
        return Err(stages::EvaluationFailure::Infrastructure(format!(
            "browser evaluator receipt has no boolean passed field; stderr: {stderr}"
        ))
        .into());
    }
    Ok(receipt)
}

#[test]
fn missing_or_invalid_browser_receipts_are_infrastructure_failures() {
    let evidence = tempfile::tempdir().unwrap();
    for content in [None, Some("not JSON"), Some("null")] {
        if let Some(content) = content {
            std::fs::write(evidence.path().join("browser.json"), content).unwrap();
        }
        let error = read_browser_receipt(evidence.path(), "Chrome unavailable").unwrap_err();
        assert!(matches!(
            error.downcast_ref::<stages::EvaluationFailure>(),
            Some(stages::EvaluationFailure::Infrastructure(_))
        ));
        assert!(error.to_string().contains("Chrome unavailable"));
    }
}

#[tokio::test]
#[ignore = "requires Google Chrome and repository npm ci; run make test-evals-browser"]
async fn browser_checker_accepts_static_fixture_without_live_inference() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("README.md"), "Open index.html.").unwrap();
    std::fs::write(project.join("index.html"), "<!doctype html><title>Test</title><button onclick=\"document.body.style.background='black'\">Toggle night</button>").unwrap();
    check_pagoda_browser(&project, &root.path().join("evidence"))
        .await
        .unwrap();
}
