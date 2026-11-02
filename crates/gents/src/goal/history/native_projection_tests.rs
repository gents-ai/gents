use super::*;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::identity::NodeIdentity;
use std::io::Write;
use std::sync::{Arc, Mutex};

fn model(facts: &[ReuseFacts]) -> Vec<bool> {
    let mut source = tempfile::Builder::new().suffix(".lean").tempfile().unwrap();
    let input = serde_json::to_string(&serde_json::to_string(facts).unwrap()).unwrap();
    write!(source, "import Proofs.GoalAutomation.HistoryProjection\ndef main : IO Unit := do\n  let input ← IO.ofExcept (Lean.Json.parse {input})\n  let result ← IO.ofExcept (GoalAutomation.HistoryProjection.evaluate input)\n  IO.println result.compress\n").unwrap();
    let output = gents_lean_contract::run_prepared_generator(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs"),
        "Proofs.GoalAutomation.HistoryProjection:olean",
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

struct NativeReader<'txn, 'node> {
    txn: &'txn ConfigApplyTxn<'node>,
    full: bool,
    cids: Mutex<Vec<String>>,
    facts: Mutex<Vec<ReuseFacts>>,
}

impl<'txn, 'node> NativeReader<'txn, 'node> {
    fn new(txn: &'txn ConfigApplyTxn<'node>, full: bool) -> Self {
        Self {
            txn,
            full,
            cids: Mutex::new(Vec::new()),
            facts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ConfigRead for NativeReader<'_, '_> {
    async fn execute_read(
        &self,
        query: &str,
    ) -> std::result::Result<serde_json::Value, crate::store::config_client::ConfigClientError>
    {
        let mut response = self.txn.execute(query).await?;
        if let Some((_, rest)) = query.split_once("cid: [") {
            let raw = rest.split_once(']').unwrap().0;
            let cids: Vec<String> = serde_json::from_str(&format!("[{raw}]"))?;
            self.cids.lock().unwrap().extend(cids);
        }
        if let Some(commits) = response
            .pointer_mut("/data/_commits")
            .and_then(serde_json::Value::as_array_mut)
        {
            for commit in commits {
                let decoded: GoalCompositeCommit = serde_json::from_value(commit.clone())?;
                self.facts.lock().unwrap().push(decoded.reuse_facts());
                if self.full {
                    commit.as_object_mut().unwrap().remove("links");
                }
            }
        }
        Ok(response)
    }
}

#[tokio::test]
async fn native_goal_projection_matches_full_history_with_fewer_reconstructions() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("node.key");
    let key = tokio::task::spawn_blocking(move || crate::KeyIdentity::load_or_create(path, None))
        .await
        .unwrap()
        .unwrap();
    let owner = key.did();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(owner)
            .build()
            .await
            .unwrap(),
    );
    let access = ConfigAccess::local(Arc::clone(&node));
    access
        .add_collection(gents_protocol::schemas::GOAL)
        .await
        .unwrap();
    let id = ConfigAccess::transact_local_write(&node, None, "test.goal_projection.seed", |txn| {
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: GoalMutationInputArg!) {create_Goal(input:$input) {_docID}}",
                &serde_json::json!({"input":{"goal_id":"goal","node_did":owner,"session_id":"chat", "status":"active","objective":"retained objective","assignment_root_request_doc_id":"physical-root"}}),
            ).await?;
            crate::store::graphql::created_doc_id(&response,"Goal")
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
    let (profile, facts) = ConfigAccess::transact_local_readonly(&node, None, "test.goal_projection.compare", |txn| {
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
    node.shutdown().await;
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
