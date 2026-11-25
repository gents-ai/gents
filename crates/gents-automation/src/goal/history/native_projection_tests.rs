use super::*;
use crate::test_support::doubles::NativeReader;
use gents_store::config_client::ConfigAccess;
use std::io::Write;

fn model(facts: &[ReuseFacts]) -> Vec<bool> {
    let mut source = tempfile::Builder::new().suffix(".lean").tempfile().unwrap();
    let input = serde_json::to_string(&serde_json::to_string(facts).unwrap()).unwrap();
    write!(source, "import Proofs.Automation.GoalAutomation.HistoryProjection\ndef main : IO Unit := do\n  let input ← IO.ofExcept (Lean.Json.parse {input})\n  let result ← IO.ofExcept (GoalAutomation.HistoryProjection.evaluate input)\n  IO.println result.compress\n").unwrap();
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Automation.GoalAutomation.HistoryProjection:olean",
        source.path().to_str().unwrap(),
    )
    .unwrap();
    serde_json::from_str(output.trim()).unwrap()
}

#[test]
fn native_projection_reuse_follows_the_executed_model() {
    let facts = (0..4)
        .flat_map(|parent_count| {
            [false, true]
                .into_iter()
                .flat_map(move |metadata_complete| {
                    [false, true]
                        .into_iter()
                        .map(move |assignment_written| ReuseFacts {
                            parent_count,
                            metadata_complete,
                            assignment_written,
                        })
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        facts.iter().map(ReuseFacts::can_reuse).collect::<Vec<_>>(),
        model(&facts)
    );
}

#[tokio::test]
async fn native_goal_projection_matches_full_history_with_fewer_reconstructions() {
    let signed_home = gents_documents::test_support::SignedHome::start(
        "goal-history-projection",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, _client) = signed_home.into_parts();
    let node = db.node.clone();
    let owner = db.node_identity.did();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let admin = ConfigAccess::local_as(node.clone(), ::identity::Did::new(owner).unwrap());
    let agent_did = ::identity::Did::new(agent.did()).unwrap();
    for permission in [
        gents_store::config_client::NodePermission::CollectionGet,
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
    ] {
        admin
            .grant_node_permission(&agent_did, permission)
            .await
            .unwrap();
    }
    let access =
        ConfigAccess::local_managed_agent(node.clone(), agent_did, agent.as_ref()).unwrap();
    let id = access.transact_write("test.goal_projection.seed", |txn| {
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: GoalMutationInputArg!) {create_Goal(input:$input) {_docID}}",
                &serde_json::json!({"input":{"goal_id":"goal","node_did":owner,"session_id":"chat", "status":"active","objective":"retained objective","assignment_root_request_doc_id":"physical-root"}}),
            ).await?;
            gents_store::graphql::created_doc_id(&response,"Goal")
        })
    }).await.unwrap();
    for tokens in 1..=66 {
        access
            .write(
                "test.goal_projection.usage",
                &format!(
            "mutation {{update_Goal(docID:\"{}\",input:{{tokens_used:{tokens}}}) {{_docID}}}}",
            escape_graphql_string(&id),
        ),
            )
            .await
            .unwrap();
    }
    let (profile, facts) = access.transact_readonly("test.goal_projection.compare", |txn| {
        Box::pin(async move {
            let baseline = NativeReader::new(txn, true);
            let started = std::time::Instant::now();
            let full = load_assignment_versions_for_session(&baseline, owner, "chat").await?;
            let baseline_us = started.elapsed().as_micros();
            let optimized = NativeReader::new(txn, false);
            let started = std::time::Instant::now();
            let projected = load_assignment_versions_for_session(&optimized, owner, "chat").await?;
            let projected_us = started.elapsed().as_micros();
            assert_eq!(full.len(), 67);
            assert_eq!(full.len(), projected.len());
            for (expected, actual) in full.iter().zip(&projected) {
                assert_eq!(actual.identity, expected.identity);
                assert_eq!(actual.commit_cid, expected.commit_cid);
                assert_eq!(actual.height, expected.height);
                assert_eq!(actual.parent_commit_cids, expected.parent_commit_cids);
            }
            assert_eq!(serde_json::to_value(super::super::request_assignment::latest_assignment_goals(&full)?)?,
                serde_json::to_value(super::super::request_assignment::latest_assignment_goals(&projected)?)?);
            let full_count = baseline.cids.lock().unwrap().len();
            let projected_count = optimized.cids.lock().unwrap().len();
            assert_eq!(full_count, 67);
            assert_eq!(projected_count, 2);
            Ok::<_, anyhow::Error>((
                serde_json::json!({"revisions":full.len(),"baseline_reconstructions":full_count,
                    "projected_reconstructions":projected_count,"baseline_us":baseline_us,"projected_us":projected_us}),
                optimized.facts.into_inner().unwrap(),
            ))
        })
    }).await.unwrap();
    node.shutdown().await.expect("node shutdown");
    let native = facts.iter().map(ReuseFacts::can_reuse).collect::<Vec<_>>();
    assert_eq!(
        native,
        tokio::task::spawn_blocking(move || model(&facts))
            .await
            .unwrap()
    );
    if let Some(path) = std::env::var_os("GENTS_GOAL_HISTORY_PROFILE") {
        tokio::fs::write(path, serde_json::to_vec_pretty(&profile).unwrap())
            .await
            .unwrap();
    }
}
