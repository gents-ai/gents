use std::path::Path;

use anyhow::{ensure, Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::{json, Value};

use super::live_gate::HeldTool;

pub async fn gates(root: &Path, scenario: Option<&str>) -> Result<Vec<HeldTool>> {
    let count = match scenario {
        Some("live-goal-removal") => 2,
        Some("live-goal-replacement") => 3,
        Some("live-goal-delivery") => 1,
        _ => 0,
    };
    let mut gates = Vec::with_capacity(count);
    for _ in 0..count {
        gates.push(HeldTool::start(root).await?);
    }
    Ok(gates)
}

pub async fn prepare(root: &Path, scenario: &str, gates: &[HeldTool]) -> Result<()> {
    let descriptors = gates.iter().map(HeldTool::descriptor).collect::<Vec<_>>();
    let objectives = gates.iter().enumerate().map(|(index, gate)| format!(
        "Exercise goal {}. {} After the tool returns, answer briefly without changing the Goal status.", index + 1, gate.prompt()
    )).collect::<Vec<_>>();
    let (name, fixture) = match scenario {
        "live-goal-removal" => (
            "editor-goal-removal-fixture.json",
            json!({"objectives":objectives,"gates":descriptors}),
        ),
        "live-goal-replacement" => {
            tokio::fs::create_dir_all(root.join("goal-observer")).await?;
            tokio::fs::write(root.join("context.rs"), "replacement disk context").await?;
            tokio::fs::write(
                root.join("goal-observer/context.rs"),
                "observer disk context",
            )
            .await?;
            (
                "editor-goal-replacement-fixture.json",
                json!({"objectives":objectives,"gates":descriptors,
                    "draft":"Keep this unsent replacement draft",
                    "queued":"Run this queued follow-up before replacing the goal",
                    "followup":"Complete the second goal from this follow-up",
                    "observerDraft":"Keep this observer replacement draft",
                    "observerEdit":"Keep this stale replacement goal edit",
                    "capture":"replacement goal captured selection",
                    "observerCapture":"private observer replacement selection",
                    "captureFile":"context.rs","observerCaptureFile":"goal-observer/context.rs",
                    "captureDisk":"replacement disk context","observerCaptureDisk":"observer disk context",
                    "busyReason":"wait for this session to finish before starting a goal"}),
            )
        }
        "live-goal-delivery" => {
            tokio::fs::write(root.join("goal-delivery.rs"), "Original disk contents.\n").await?;
            (
                "editor-goal-delivery-fixture.json",
                json!({"objective":format!("Recover this installed Goal without replacing its input. {} When this Goal is later resumed, call update_goal with status complete.", gates[0].prompt()),
                    "capture":"unsaved Goal delivery context\n",
                    "draft":"Keep this newer Goal delivery draft.","gate":descriptors[0],
                    "image":{"name":"goal.png","mediaType":"image/png","data":super::super::goal_delivery::IMAGE_DATA}}),
            )
        }
        _ => return Ok(()),
    };
    tokio::fs::write(root.join(name), serde_json::to_vec(&fixture)?).await?;
    Ok(())
}

pub async fn verify(
    access: &ConfigAccess,
    root: &Path,
    node: &str,
    author: &str,
    scenario: &str,
) -> Result<Value> {
    let suffix = scenario
        .strip_prefix("live-")
        .context("live Goal scenario")?;
    let name = if scenario == "live-goal-delivery" {
        "editor-live-goal-delivery-ui.json".to_owned()
    } else {
        format!("editor-{suffix}-ui.json")
    };
    let ui: Value = serde_json::from_slice(&tokio::fs::read(root.join(name)).await?)?;
    let observed = access.execute(&format!(
        "{{Goal(filter: {{node_did: {{_eq: \"{}\"}}}}) {{{}}} AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, purpose: {{_eq: \"normal\"}}}}) {{{} interrupt_requested_at}}}}",
        escape_graphql_string(node), gents_protocol::goal::GOAL_FIELDS,
        escape_graphql_string(node), gents::SIGNED_REQUEST_FIELDS,
    )).await?;
    let goals = observed["data"]["Goal"]
        .as_array()
        .context("durable goals")?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(observed["data"]["AgentRequest"].clone())?;
    let objectives = if scenario == "live-goal-delivery" {
        vec![ui["objective"].as_str().context("delivered objective")?]
    } else {
        ui["objectives"]
            .as_array()
            .context("controlled objectives")?
            .iter()
            .map(|value| value.as_str().context("controlled objective"))
            .collect::<Result<Vec<_>>>()?
    };
    for (index, objective) in objectives.iter().enumerate() {
        let goal = goals
            .iter()
            .find(|goal| goal["objective"] == *objective)
            .context("persisted controlled goal")?;
        let completed =
            scenario == "live-goal-delivery" || (scenario == "live-goal-replacement" && index == 1);
        ensure!(goal["status"] == if completed { "complete" } else { "stopped" });
        let opening = rows
            .iter()
            .find(|row| row.content.as_deref() == Some(*objective))
            .context("signed Goal opening")?;
        verify_request(opening, author)?;
        ensure!(
            opening.interrupt_requested_at.is_none(),
            "Goal control interrupted its owned work"
        );
        if !completed {
            ensure!(goal["completion_evidence"].is_null());
        }
        if scenario == "live-goal-delivery" {
            let input = opening.input.as_ref().context("captured Goal input")?;
            ensure!(input
                .captured_context
                .iter()
                .any(|capture| Some(capture.content.as_str()) == ui["capture"].as_str()));
            ensure!(
                input.attachments.len() == 1
                    && Some(input.attachments[0].data.as_str()) == ui["image"]["data"].as_str()
            );
        }
    }
    for key in ["queued", "followup"] {
        if let Some(content) = ui[key].as_str() {
            let row = rows
                .iter()
                .find(|row| row.content.as_deref() == Some(content))
                .context("preserved queued follow-up")?;
            verify_request(row, author)?;
        }
    }
    if let Some(draft) = ui["draft"].as_str() {
        ensure!(
            rows.iter().all(|row| row.content.as_deref() != Some(draft)),
            "Goal controls submitted the draft"
        );
    }
    let evidence = json!({"ui":ui,"goals":goals,"liveModel":true});
    tokio::fs::write(
        root.join(format!("editor-{suffix}-canonical.json")),
        serde_json::to_vec(&evidence)?,
    )
    .await?;
    Ok(evidence)
}

fn verify_request(row: &AgentRequestRow, author: &str) -> Result<()> {
    gents_store::identity::request_receipt::verify_request_receipt_signature(row)?;
    ensure!(
        row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && row.admission_signer_did.as_deref() == Some(author)
            && row.requester_did.as_deref() == Some(author)
    );
    Ok(())
}
