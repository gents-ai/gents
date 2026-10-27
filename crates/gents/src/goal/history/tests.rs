use super::*;
use serde_json::{json, Value};
use std::sync::Mutex;

struct HistoryReader {
    calls: Mutex<Vec<Vec<String>>>,
    fault: Option<&'static str>,
}

#[async_trait::async_trait]
impl ConfigRead for HistoryReader {
    async fn execute_read(&self, query: &str) -> Result<Value> {
        if query.contains("_commits(") {
            return Ok(json!({"data":{"_commits":(0..67).rev().map(|index| json!({
                "cid":format!("commit-{index:03}"), "height":index + 1, "fieldName":"_C",
                "heads":if index == 0 {vec![]} else {vec![json!({"cid":format!("commit-{:03}",index-1),"fieldName":"_C"})]}
            })).collect::<Vec<_>>()}}));
        }
        let Some((_, suffix)) = query.split_once("cid: [") else {
            return Ok(json!({"data":{"Goal":[{"_docID":"goal-document"}]}}));
        };
        let (raw, _) = suffix.split_once(']').unwrap();
        let cids: Vec<String> = serde_json::from_str(&format!("[{raw}]"))?;
        let batch = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(cids.clone());
            calls.len()
        };
        if batch == 2 && self.fault == Some("unavailable") {
            anyhow::bail!("retained block unavailable");
        }
        let mut rows = cids.iter().map(|cid| json!({
            "_docID":"goal-document", "node_did":"owner", "session_id":"chat", "goal_id":"goal",
            "status":"active", "objective":cid, "assignment_root_request_doc_id":"physical-root"
        })).collect::<Vec<_>>();
        if batch == 2 && self.fault == Some("omitted") {
            rows.pop();
        }
        if batch == 2 && self.fault == Some("foreign") {
            rows[0]["node_did"] = json!("other-owner");
        }
        Ok(json!({"data":{"Goal":rows}}))
    }
}

#[tokio::test]
async fn retained_history_is_complete_across_bounded_reconstruction_queries() {
    let reader = HistoryReader {
        calls: Mutex::new(Vec::new()),
        fault: None,
    };
    let versions = load_goal_versions_for_session(&reader, "owner", "chat")
        .await
        .unwrap();
    assert_eq!(versions.len(), 67);
    for (index, version) in versions.iter().enumerate() {
        assert_eq!(version.commit_cid, format!("commit-{index:03}"));
        assert_eq!(version.goal.objective, version.commit_cid);
        assert_eq!(version.height, index as i64 + 1);
        assert_eq!(
            version.parent_commit_cids,
            if index == 0 {
                vec![]
            } else {
                vec![format!("commit-{:03}", index - 1)]
            }
        );
    }
    let calls = reader.calls.lock().unwrap();
    assert!(calls.len() > 1);
    assert!(calls.iter().all(|batch| batch.len() <= 32));
    assert_eq!(calls.iter().flatten().count(), versions.len());
    let latest = super::super::latest_assignment_goals(&versions).unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].objective, "commit-066");
}

#[tokio::test]
async fn unavailable_or_incomplete_batches_never_return_partial_assignment_history() {
    for (fault, expected) in [
        ("unavailable", "batch 2"),
        ("omitted", "snapshots for 32 visible commits"),
        ("foreign", "belongs to a different owner"),
    ] {
        let reader = HistoryReader {
            calls: Mutex::new(Vec::new()),
            fault: Some(fault),
        };
        let error = load_goal_versions_for_session(&reader, "owner", "chat")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        if fault == "unavailable" {
            assert!(format!("{error:#}").contains("retained block unavailable"));
        }
    }
}
