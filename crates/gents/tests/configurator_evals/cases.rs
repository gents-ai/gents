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
    stages::prepare_stage(activation, node, owner, setup, "skill-preview").await?;
    let before = super::onboarding_scenarios::preview_snapshot(node).await?;
    super::reporting::write_json_new(&evidence.join("skill-preview-before.json"), &before)?;
    let previewed = stages::execute(
        activation,
        node,
        owner,
        setup,
        "skill-preview",
        &include_str!("../fixtures/configurator_evals/skill_setup.md")
            .trim_end()
            .replace("{{SKILL_DIRECTORY}}", &source.to_string_lossy()),
        evidence,
    )
    .await?;
    previewed.ensure_completed()?;
    activation.wait().await?;
    let after = super::onboarding_scenarios::preview_snapshot(node).await?;
    super::reporting::write_json_new(&evidence.join("skill-preview-after.json"), &after)?;
    ensure!(
        before == after,
        "skill preview mutated canonical configuration or schema"
    );
    super::onboarding_scenarios::assert_preview_calls(
        &super::onboarding_scenarios::tool_calls(node, &previewed.request_id).await?,
    )?;
    ensure!(
        builder_before == behavior_configuration(node, owner, None).await?
            && setup_before == behavior_configuration(node, owner, Some(setup)).await?,
        "skill preview changed configuration before approval"
    );
    let skill_query = format!(
        r#"{{ Skill(filter: {{agent_did: {{_eq: "{}"}}, skill_id: {{_eq: "eval-coding-check"}}}}) {{source_directory}} }}"#,
        gents::graphql::escape_graphql_string(owner)
    );
    ensure!(
        rows(node, &skill_query, "Skill").await?.is_empty(),
        "skill preview imported before approval"
    );
    let configured = stages::execute(
        activation,
        node,
        owner,
        setup,
        "skill-approve",
        &include_str!("../fixtures/configurator_evals/skill_approve.md")
            .trim_end()
            .replace("{{SKILL_DIRECTORY}}", &source.to_string_lossy()),
        evidence,
    )
    .await?;
    configured.ensure_completed()?;
    activation.wait().await?;
    let imported = rows(node, &skill_query, "Skill").await?;
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

#[test]
fn skill_workflow_separates_preview_from_scoped_approval() {
    let preview = include_str!("../fixtures/configurator_evals/skill_setup.md");
    let approval = include_str!("../fixtures/configurator_evals/skill_approve.md");
    assert!(preview.contains("Do not apply any changes yet"));
    assert!(approval.contains("Apply those changes now"));
    for prompt in [preview, approval] {
        assert!(prompt.contains("{{SKILL_DIRECTORY}}"));
        assert!(prompt.contains("eval-coding-check"));
        assert!(prompt.contains("Builder"));
        assert!(prompt.contains("skill's procedure"));
    }
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
    for message in ["Hello steward", "the garden is green"] {
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
        let source_doc_id =
            gents::graphql::single_mutation_document(&response, "create_EvalAutomationInput")?
                .and_then(|row| row["_docID"].as_str())
                .context("automation input document ID missing")?;
        receipts.push(serde_json::json!({
            "input": message, "correlation": correlation,
            "source_doc_id": source_doc_id, "submission": response.data, "output": null,
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
        let requests = rows(node, &format!(r#"{{ AgentRequest(filter: {{agent_did: {{_eq: "{escaped_owner}"}}}}) {{request_id caused_by_source_doc_id caused_by_trigger_id lifecycle_state}} }}"#), "AgentRequest").await?;
        let requests = requests
            .iter()
            .filter(|request| trigger_ids.contains(&request["caused_by_trigger_id"]))
            .collect::<Vec<_>>();
        let each_input_invoked = receipts.iter().all(|receipt| {
            receipt["source_doc_id"].as_str().is_some_and(|id| {
                requests
                    .iter()
                    .any(|request| request["caused_by_source_doc_id"] == id)
            })
        });
        if each_input_invoked
            && requests.iter().all(|request| {
                gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
                    request["lifecycle_state"].as_str(),
                )
            })
        {
            ensure!(requests.iter().all(|request| request["lifecycle_state"] == "completed"),
                "automation produced output but its requests did not complete successfully: {requests:?}");
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
