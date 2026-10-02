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
