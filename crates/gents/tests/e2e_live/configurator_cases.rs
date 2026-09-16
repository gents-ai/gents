//! Independent acceptance checks for progressively generated behaviors.

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
        node, agent_did, builder, "builder-readiness",
        &format!("Verify this new coding workspace by doing the work now. Inside {user_home}, create a directory named readiness. Write readiness/test.sh containing exactly:\n#!/bin/sh\nset -eu\ntest \"$((2 + 2))\" -eq 4\nprintf 'BUILD_TEST_OK\\n' > \"$(dirname \"$0\")/result.txt\"\n\nExecute it with sh using your command tool. Read result.txt and report the test outcome. Do not write result.txt yourself; the script must produce it. No network or dependencies are needed."),
        evidence,
    ).await?;
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
            r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}}}) {{tool_name lifecycle_state args}} }}"#,
            gents::graphql::escape_graphql_string(&result.request_id)
        ),
        "AgentToolCall",
    )
    .await?;
    ensure!(
        calls
            .iter()
            .any(|call| call["tool_name"] == "bash_unrestricted"
                && call["lifecycle_state"] == "completed"
                && call["args"]
                    .as_str()
                    .is_some_and(|args| args.contains("readiness/test.sh"))),
        "Builder did not successfully execute its command tool"
    );
    Ok(())
}

pub(super) async fn verify_pagoda_sequence(
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
    let prompt = |text: &str| text.replace("{{USER_HOME}}", &workspace.to_string_lossy());
    let creation = stages::checked("pagoda", evidence, async {
        let created = stages::execute(
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
    })
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
    let before = project_snapshot(workspace)?;
    retain_project(
        &project,
        &evidence.join("pagoda-source"),
        &project_snapshot(&project)?,
    )?;
    let review = stages::checked("review", evidence, async {
        let review = stages::execute(
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
    .await?;
    let improvement = stages::checked("improve", evidence, async {
        let improved = stages::execute(
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
        check_pagoda_browser(&project, &evidence.join("improve-browser")).await?;
        retain_project(
            &project,
            &evidence.join("improve-source"),
            &project_snapshot(&project)?,
        )?;
        Ok(())
    })
    .await;
    creation.and(improvement)
}

pub(super) async fn verify_skill_workflow(
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
        "---\nname: coding-check\ndescription: Run the configured coding readiness procedure\n---\nFor the configured readiness procedure, read skill-fixture/references/checklist.md within your working root and carry out its instructions.\n")?;
    let marker = format!("SKILL_CHECK_{}", uuid::Uuid::new_v4());
    std::fs::write(source.join("references/checklist.md"),
        format!("Write readiness/skill-check.txt containing exactly {marker} followed by a newline. Read it back and report the result.\n"))?;
    let configured = stages::execute(node, owner, setup, "skill-setup",
        &format!("Import the standard skill directory {} as skill ID eval-coding-check and attach it to the existing Builder behavior. Preserve Builder's other context, root and tool settings. Preview first and verify the persisted attachment. Do not perform the skill's procedure yourself.", source.display()), evidence).await?;
    configured.ensure_completed()?;
    stages::wait_for_config_activation(node, owner, &configured.request_id).await?;
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
        node,
        owner,
        builder,
        "skill-use",
        "Load your attached coding-check skill and perform its configured readiness procedure now.",
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
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    setup: &str,
    evidence: &std::path::Path,
) -> Result<()> {
    let result = run_document_automation(node, owner, setup, evidence).await;
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
    if let Some(requests) = diagnostics
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
    {
        for (index, request) in requests.iter().enumerate() {
            // Names are not ownership evidence: the model may legitimately
            // choose an eval-trigger-* ID for its own workflow too.
            if request["caused_by_trigger_id"].as_str().is_some() {
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
    result
}

async fn run_document_automation(
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
    setup: &str,
    evidence: &std::path::Path,
) -> Result<()> {
    let setup_before = behavior_configuration(node, owner, Some(setup)).await?;
    let mut builder_before = behavior_configuration(node, owner, None).await?;
    let configured = stages::execute(
        node,
        owner,
        setup,
        "automation-setup",
        include_str!("../fixtures/configurator_evals/document_automation.md"),
        evidence,
    )
    .await?;
    configured.ensure_completed()?;
    stages::wait_for_config_activation(node, owner, &configured.request_id).await?;
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
    let sources = rows(node, &format!(r#"{{ EventSource(filter: {{agent_did: {{_eq: "{escaped_owner}"}}, source_collection: {{_eq: "EvalAutomationInput"}}}}) {{event_source_id}} }}"#), "EventSource").await?;
    let triggers = rows(node, &format!(r#"{{ Trigger(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{trigger_id source}} }}"#), "Trigger").await?;
    let trigger_ids = triggers
        .iter()
        .filter(|trigger| {
            sources
                .iter()
                .any(|source| trigger["source"]["event_source_id"] == source["event_source_id"])
        })
        .map(|trigger| trigger["trigger_id"].clone())
        .collect::<Vec<_>>();
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
    .await
    .context("browser evaluator timed out")??;
    std::fs::write(
        evidence.join("process.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))?,
    )?;
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(evidence.join("browser.json"))?)?;
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

#[tokio::test]
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
