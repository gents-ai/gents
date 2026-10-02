use std::path::Path;

use gents::document_config::EvalSplit;
use gents::eval::checks::CheckRegistry;
use gents::pack::{load_pack_config, PackInstallOptions, PackManifest};

#[test]
fn session_investigation_eval_pack_validates_all_splits_and_shipped_checks() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/configurator_evals/sessions");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:eval-owner".into(),
        },
        &|path| Ok(std::fs::read(root.join(path))?),
        &|_| None,
    )
    .unwrap();
    let definition = &config.eval_definitions[0];
    definition.validate().unwrap();
    let registry = CheckRegistry::builtin();
    for split in [EvalSplit::Train, EvalSplit::Validation, EvalSplit::HeldOut] {
        assert!(definition.cases.iter().any(|case| case.split == split));
    }
    for case in &definition.cases {
        for stage in &case.stages {
            assert!(stage.deadline_secs <= 600);
            for check in &stage.checks {
                assert!(
                    registry.get(&check.check).is_some(),
                    "unknown sessions check {}",
                    check.check
                );
            }
        }
    }
    let old = definition
        .cases
        .iter()
        .find(|case| case.case_id == "discover-old")
        .unwrap();
    assert!(
        old.fixtures
            .as_ref()
            .unwrap()
            .documents
            .iter()
            .filter(|doc| doc.collection == "AgentRequest")
            .count()
            > 5000
    );
}

#[tokio::test]
#[ignore = "requires GENTS_SESSION_METRICS_RUN and GENTS_SESSION_METRICS_OUTPUT retained eval paths"]
async fn retained_session_eval_output_metrics_use_native_canonical_reader() -> anyhow::Result<()> {
    use std::sync::Arc;

    use anyhow::Context;
    use gents::config_client::ConfigAccess;
    use gents::defra_node::EmbeddedNode;
    use gents::session::load_tool_call_presentation;
    use gents::{AgentIdentity, KeyIdentity};
    use serde_json::json;

    let run = std::path::PathBuf::from(std::env::var("GENTS_SESSION_METRICS_RUN")?);
    let output = std::path::PathBuf::from(std::env::var("GENTS_SESSION_METRICS_OUTPUT")?);
    let mut metrics = Vec::new();
    for entry in std::fs::read_dir(run.join("trials"))? {
        let trial = entry?.path();
        let home = trial.join("home");
        if !home.is_dir() {
            continue;
        }
        let identity = KeyIdentity::load_or_create(home.join("node.key"), None)?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(&home)
                .with_node_identity_did(identity.did())
                .build()
                .await?,
        );
        let access = ConfigAccess::Local(node.clone());
        let rows = access.execute("{ AgentToolCall(filter: {tool_name: {_eq: \"sessions\"}}) { _docID agent_did session_id requester_did lifecycle_state } }").await?;
        let calls = rows["data"]["AgentToolCall"]
            .as_array()
            .context("missing canonical tool calls")?;
        let mut bytes = 0usize;
        let mut errors = 0usize;
        let mut undelivered = 0usize;
        let mut oversized = 0usize;
        let mut actionable_errors = 0usize;
        let mut protected_content = false;
        for call in calls {
            let presentation = load_tool_call_presentation(
                &access,
                call["_docID"].as_str().context("missing tool identity")?,
                call["agent_did"].as_str().context("missing principal")?,
                call["session_id"].as_str().context("missing session")?,
                call["requester_did"].as_str(),
            )
            .await?;
            if let Some(result) = presentation.result {
                bytes += result.len();
                oversized += usize::from(result.len() > 64_000);
                actionable_errors += usize::from(
                    call["lifecycle_state"] == "failed" && result.contains("next call:"),
                );
                protected_content |= result.contains("Confidential partner rotation ledger");
            } else {
                undelivered += 1;
            }
            errors += usize::from(call["lifecycle_state"] == "failed");
        }
        metrics.push(json!({
            "trial": trial.file_name().context("missing trial name")?.to_string_lossy(),
            "session_tool_calls": calls.len(), "delivered_output_bytes": bytes,
            "undelivered_calls": undelivered, "failed_calls": errors,
            "oversized_results_over_64000_bytes": oversized, "errors_naming_next_call": actionable_errors,
            "protected_fixture_content_returned": protected_content,
        }));
        drop(access);
        drop(node);
    }
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&json!({"run": run, "trials": metrics}))?,
    )?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires GENTS_EVAL_TRACE_HOME, GENTS_EVAL_TRACE_DID and GENTS_EVAL_TRACE_OUTPUT"]
async fn retained_tool_trace_uses_native_canonical_reader() -> anyhow::Result<()> {
    use std::sync::Arc;

    use anyhow::Context;
    use gents::config_client::ConfigAccess;
    use gents::defra_node::EmbeddedNode;
    use gents::graphql::escape_graphql_string;
    use gents::session::load_tool_call_presentation;
    use gents::{AgentIdentity, KeyIdentity};
    use serde_json::json;

    let home = std::path::PathBuf::from(std::env::var("GENTS_EVAL_TRACE_HOME")?);
    let did = std::env::var("GENTS_EVAL_TRACE_DID")?;
    let output = std::path::PathBuf::from(std::env::var("GENTS_EVAL_TRACE_OUTPUT")?);
    anyhow::ensure!(
        home.join("node.key").is_file(),
        "retained trial identity is missing"
    );
    let identity = KeyIdentity::load_or_create(home.join("node.key"), None)?;
    anyhow::ensure!(
        identity.did() == did,
        "trace DID does not own retained trial home"
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&home)
            .with_node_identity_did(identity.did())
            .build()
            .await?,
    );
    let access = ConfigAccess::Local(node);
    let response = access.execute(&format!("{{AgentToolCall(filter: {{agent_did: {{_eq: \"{}\"}}}}, order: {{started_at: ASC}}) {{_docID agent_did session_id requester_did tool_name lifecycle_state started_at}}}}",escape_graphql_string(&did))).await?;
    let rows = response["data"]["AgentToolCall"]
        .as_array()
        .context("missing canonical tool calls")?;
    let mut calls = Vec::new();
    for row in rows {
        let presentation = load_tool_call_presentation(
            &access,
            row["_docID"]
                .as_str()
                .context("missing physical tool identity")?,
            &did,
            row["session_id"].as_str().context("missing tool session")?,
            row["requester_did"].as_str(),
        )
        .await?;
        calls.push(json!({"tool_call":row,"arguments":presentation.arguments,"result":presentation.result,"live_output":presentation.live_output}));
    }
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&json!({"home":home,"agent_did":did,"calls":calls}))?,
    )?;
    Ok(())
}

#[test]
fn transcript_citation_capture_is_scoped_to_the_trial_session() {
    let case: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/configurator_evals/sessions/cases/transcript_evidence.json"
    ))
    .unwrap();
    let captures: Vec<_> = case["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|stage| stage["capture"].as_array().into_iter().flatten())
        .filter(|capture| capture["name"] == "source_headers")
        .collect();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0]["filter"]["session_id"]["_eq"], "$session");
    assert_eq!(captures[0]["filter"]["sequence"]["_eq"], 1);
}
