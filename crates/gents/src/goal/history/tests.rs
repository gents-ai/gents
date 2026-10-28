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
        if batch == 2 && self.fault == Some("projection") {
            rows[0]["status"] = json!("paused");
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
    let projected = versions
        .into_iter()
        .map(|version| {
            GoalAssignmentVersion::from_snapshot(
                version.goal,
                version.commit_cid,
                version.height,
                version.parent_commit_cids,
            )
        })
        .collect::<Vec<_>>();
    let latest = super::super::latest_assignment_goals(&projected).unwrap();
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

struct UnchangedAssignmentReader {
    inner: HistoryReader,
    metadata_fault: Option<&'static str>,
}

#[async_trait::async_trait]
impl ConfigRead for UnchangedAssignmentReader {
    async fn execute_read(&self, query: &str) -> Result<Value> {
        let mut response = self.inner.execute_read(query).await?;
        if let Some(commits) = response
            .pointer_mut("/data/_commits")
            .and_then(Value::as_array_mut)
        {
            for commit in commits {
                let height = commit["height"].as_u64().unwrap();
                commit["links"] = json!([{
                    "cid":format!("field-{height}"),
                    "height":height,
                    "fieldName":if height == 1 {"goal_id"} else {"tokens_used"}
                }]);
                for head in commit["heads"].as_array_mut().unwrap() {
                    head["height"] = json!(height - 1);
                }
                if height == 21 {
                    match self.metadata_fault {
                        Some("missing_link") => {
                            commit["links"][0].as_object_mut().unwrap().remove("height");
                        }
                        Some("unknown_link") => {
                            commit["links"][0]["fieldName"] = json!("future_field")
                        }
                        Some("immutable") => commit["links"][0]["fieldName"] = json!("_I"),
                        Some("assignment") => commit["links"][0]["fieldName"] = json!("status"),
                        Some("merge") => commit["heads"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"cid":"commit-000", "height":1,"fieldName":"_C"})),
                        Some("missing_parent") => commit["heads"][0]["cid"] = json!("unavailable"),
                        Some("parent_height") => commit["heads"][0]["height"] = json!(1),
                        _ => {}
                    }
                }
            }
        }
        Ok(response)
    }
}

#[tokio::test]
async fn usage_only_revisions_preserve_the_dag_without_reconstructing_every_snapshot() {
    let reader = UnchangedAssignmentReader {
        metadata_fault: None,
        inner: HistoryReader {
            calls: Mutex::new(Vec::new()),
            fault: None,
        },
    };
    let versions = load_assignment_versions_for_session(&reader, "owner", "chat")
        .await
        .unwrap();
    assert_eq!(versions.len(), 67);
    let latest = super::super::latest_assignment_goals(&versions).unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].objective, "commit-066");
    let calls = reader.inner.calls.lock().unwrap();
    let requested = calls.iter().flatten().collect::<Vec<_>>();
    assert_eq!(
        requested,
        [&"commit-000".to_owned(), &"commit-066".to_owned()]
    );
}

#[tokio::test]
async fn unknown_or_changed_projection_metadata_requires_native_reconstruction() {
    for fault in [
        "missing_link",
        "unknown_link",
        "immutable",
        "assignment",
        "merge",
    ] {
        let reader = UnchangedAssignmentReader {
            metadata_fault: Some(fault),
            inner: HistoryReader {
                calls: Mutex::new(Vec::new()),
                fault: None,
            },
        };
        let versions = load_assignment_versions_for_session(&reader, "owner", "chat")
            .await
            .unwrap();
        assert_eq!(versions.len(), 67);
        let calls = reader.inner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["commit-000", "commit-020", "commit-066"],
            "{fault}"
        );
    }
}

#[tokio::test]
async fn projection_failures_never_supply_partial_assignment_evidence() {
    for (fault, expected) in [
        ("unavailable", "retained block unavailable"),
        ("omitted", "snapshots for 1 visible commits"),
        ("foreign", "belongs to a different owner"),
        ("projection", "disagrees with its native frontier"),
        ("missing_parent", "history is incomplete"),
        ("parent_height", "parent metadata disagrees"),
    ] {
        let reader = UnchangedAssignmentReader {
            metadata_fault: Some(fault),
            inner: HistoryReader {
                calls: Mutex::new(Vec::new()),
                fault: Some(fault),
            },
        };
        let error = load_assignment_versions_for_session(&reader, "owner", "chat")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(expected),
            "{fault}: {error:#}"
        );
    }
}
