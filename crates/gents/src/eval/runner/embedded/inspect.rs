use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use futures::FutureExt;
use gents_protocol::row::AgentRequestRow;

use super::EmbeddedHome;
use crate::config_client::ConfigAccess;
use crate::eval::runner::TrialLocator;
use crate::graphql::escape_graphql_string;

const MAX_REQUESTS: usize = 1_000;
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, serde::Serialize)]
pub struct RetainedRequest {
    pub request_doc_id: String,
    #[serde(flatten)]
    pub request: AgentRequestRow,
}

/// Reads request content and lineage with the retained trial's existing key.
/// No runtime or schema installer is started. The blocking task owns the home
/// until shutdown, even if the caller drops its future; a query panic also
/// unwinds only after shutdown. Paths are checked before opening, so concurrent
/// replacement of a checked directory is outside this filesystem boundary.
pub async fn inspect_retained_requests(
    runs_dir: &Path,
    locator: &TrialLocator,
) -> Result<Vec<RetainedRequest>> {
    let hint = locator
        .home_hint
        .as_deref()
        .context("trial has no retained home")?;
    let trial_dir = retained_trial_dir(runs_dir, hint)?;
    let home_dir = trial_dir.join("home");
    let did = locator.trial_agent_did.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        runtime.block_on(async move {
            let home = EmbeddedHome::open_for_inspection(&home_dir, &did)
                .await
                .with_context(|| {
                    format!(
                        "opening retained trial home {} for inspection",
                        home_dir.display()
                    )
                })?;
            let result = std::panic::AssertUnwindSafe(read_requests(&home))
                .catch_unwind()
                .await;
            home.node.shutdown().await;
            match result {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })
    })
    .await
    .context("inspecting retained trial requests")?
}

pub(super) fn retained_trial_dir(runs_dir: &Path, hint: &str) -> Result<PathBuf> {
    ensure!(
        !hint.is_empty()
            && Path::new(hint)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "retained trial home hint must be a relative path without traversal"
    );
    let trial_dir = runs_dir.join(hint);
    let resolved_runs = runs_dir
        .canonicalize()
        .context("resolving the eval runs directory")?;
    for path in [
        trial_dir.clone(),
        trial_dir.join("home"),
        trial_dir.join("workspace"),
    ] {
        match path.canonicalize() {
            Ok(resolved) => ensure!(
                resolved.starts_with(&resolved_runs),
                "retained trial path {} resolves outside the eval runs directory",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("resolving {}", path.display()))
            }
        }
    }
    Ok(trial_dir)
}

async fn read_requests(home: &EmbeddedHome) -> Result<Vec<RetainedRequest>> {
    let access = ConfigAccess::Local(home.node.clone());
    let query = format!(
        r#"{{ AgentRequest(
            filter: {{ agent_did: {{ _eq: "{}" }} }},
            order: {{ created_at: ASC }}, limit: {}
        ) {{
            _docID request_id purpose agent_did requester_did behavior_id session_id
            content input lifecycle_state failure_reason created_at execution_origin
            runtime_source_request_id runtime_source_kind
            retry_parent_request retry_parent_request_doc_id retry_root_request
            superseded_by_request superseded_by_request_doc_id
            caused_by_parent_request_id caused_by_parent_request_doc_id
            caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
            caused_by_trigger_id caused_by_trigger_doc_id caused_by_trigger_kind
            caused_by_source_doc_id caused_by_correlation caused_by_trigger_context
        }} }}"#,
        escape_graphql_string(home.did()),
        MAX_REQUESTS + 1,
    );
    let response = tokio::time::timeout(READ_TIMEOUT, access.execute(&query))
        .await
        .context("retained trial request inspection timed out")??;
    decode_requests(response)
}

fn decode_requests(response: serde_json::Value) -> Result<Vec<RetainedRequest>> {
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .context("retained trial request query returned no rows array")?;
    ensure!(
        rows.len() <= MAX_REQUESTS,
        "retained trial has more than {MAX_REQUESTS} requests; inspection refuses to omit request content or lineage"
    );
    rows.iter()
        .cloned()
        .map(|row| {
            let request: AgentRequestRow =
                serde_json::from_value(row).context("decoding retained trial request")?;
            let request_doc_id = request
                .doc_id
                .clone()
                .context("retained trial request has no physical document ID")?;
            Ok(RetainedRequest {
                request_doc_id,
                request,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentIdentity, KeyIdentity};
    use serde_json::json;

    fn locator(did: &str, hint: &str) -> TrialLocator {
        TrialLocator {
            trial_agent_did: did.into(),
            session_id: "session".into(),
            home_hint: Some(hint.into()),
        }
    }

    async fn close(home: EmbeddedHome) {
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn retained_requests_preserve_content_and_lineage_under_the_trial_identity() {
        let runs = tempfile::tempdir().unwrap();
        let path = runs.path().join("trial/home");
        std::fs::create_dir_all(&path).unwrap();
        let identity = KeyIdentity::load_or_create(path.join("node.key"), None).unwrap();
        let did = identity.did().to_owned();
        let node = std::sync::Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .data_path(&path)
                .with_regolith_options(crate::storage_backend::regolith_options())
                .with_node_identity_did(&did)
                .build()
                .await
                .unwrap(),
        );
        let policy = node
            .add_dac_policy(
                &did,
                r#"
name: Retained request reader test
resources:
  - name: requests
    relations:
      - name: reader
    permissions:
      - name: read
        expr: reader
      - name: update
      - name: delete
"#,
            )
            .await
            .unwrap();
        ConfigAccess::Local(node.clone())
            .add_schema(&crate::schema::AGENT_REQUEST_SCHEMA.replace(
                "type AgentRequest ",
                &format!(
                    "type AgentRequest @policy(id: \"{}\", resource: \"requests\") ",
                    escape_graphql_string(&policy)
                ),
            ))
            .await
            .unwrap();
        let content = "original \"prompt\"\nwith full content";
        let query = format!(
            r#"mutation {{
                root: create_AgentRequest(input: {{ request_id: "root", purpose: "normal", agent_did: "{did}", session_id: "session", content: "{}", lifecycle_state: "completed", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }}
                child: create_AgentRequest(input: {{ request_id: "child", purpose: "normal", agent_did: "{did}", session_id: "child-session", content: "child prompt", lifecycle_state: "completed", created_at: "2026-01-01T00:00:01Z", retry_root_request: "root", retry_parent_request: "root", caused_by_parent_request_id: "root", caused_by_parent_tool_call_id: "call", caused_by_trigger_id: "trigger", caused_by_source_doc_id: "source", runtime_source_request_id: "root", runtime_source_kind: "session_message" }}) {{ _docID }}
                other: create_AgentRequest(input: {{ request_id: "foreign", purpose: "normal", agent_did: "did:key:other", content: "unrelated" }}) {{ _docID }}
            }}"#,
            escape_graphql_string(content),
            did = escape_graphql_string(&did),
        );
        ConfigAccess::write_local(&node, "eval.inspect.test.requests", &query)
            .await
            .unwrap();
        let denied = KeyIdentity::load_or_create(runs.path().join("denied.key"), None).unwrap();
        let original_owner_query = format!(
            r#"{{ AgentRequest(filter: {{agent_did: {{_eq: "{}"}}}}) {{content}} }}"#,
            escape_graphql_string(&did)
        );
        let denied_rows = ConfigAccess::transact_local(
            &node,
            Some(denied.did().parse().unwrap()),
            "eval.inspect.test.denied_read",
            |txn| {
                let query = original_owner_query.clone();
                Box::pin(async move { txn.execute(&query).await })
            },
        )
        .await
        .unwrap();
        assert!(denied_rows["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        node.shutdown().await;
        drop(node);
        let key = std::fs::read(path.join("node.key")).unwrap();
        let at = locator(&did, "trial");
        for _ in 0..2 {
            let requests = inspect_retained_requests(runs.path(), &at).await.unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].request.content.as_deref(), Some(content));
            assert!(!requests[0].request_doc_id.is_empty());
            let child = &requests[1].request;
            assert_eq!(child.session_id.as_deref(), Some("child-session"));
            assert_eq!(child.retry_root_request.as_deref(), Some("root"));
            assert_eq!(child.caused_by_parent_request_id.as_deref(), Some("root"));
            assert_eq!(child.caused_by_parent_tool_call_id.as_deref(), Some("call"));
            assert_eq!(child.caused_by_source_doc_id.as_deref(), Some("source"));
            assert_eq!(child.runtime_source_request_id.as_deref(), Some("root"));
            assert_eq!(std::fs::read(path.join("node.key")).unwrap(), key);
        }
    }

    #[tokio::test]
    async fn retained_requests_refuse_missing_mismatched_and_held_homes() {
        let runs = tempfile::tempdir().unwrap();
        let path = runs.path().join("trial/home");
        let home = EmbeddedHome::create_retained(&path).await.unwrap();
        let at = locator(home.did(), "trial");
        let held = inspect_retained_requests(runs.path(), &at)
            .await
            .unwrap_err();
        let held_text = format!("{held:#}");
        assert!(
            held_text.contains("already open in this process")
                || held_text.contains("already locked for read-write access"),
            "{held:#}"
        );
        close(home).await;
        let mismatch = inspect_retained_requests(runs.path(), &locator("did:key:other", "trial"))
            .await
            .unwrap_err();
        assert!(
            format!("{mismatch:#}").contains("does not match recorded DID"),
            "{mismatch:#}"
        );
        assert!(inspect_retained_requests(runs.path(), &at)
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_file(path.join("node.key")).unwrap();
        let missing = inspect_retained_requests(runs.path(), &at)
            .await
            .unwrap_err();
        assert!(
            format!("{missing:#}").contains("identity key does not exist"),
            "{missing:#}"
        );
        assert!(!path.join("node.key").exists());
        let missing =
            inspect_retained_requests(runs.path(), &locator(&at.trial_agent_did, "missing"))
                .await
                .unwrap_err();
        assert!(format!("{missing:#}").contains("is missing"), "{missing:#}");
        assert!(!runs.path().join("missing").exists());
    }

    #[tokio::test]
    async fn failed_inspection_releases_the_store_without_installing_schemas() {
        let runs = tempfile::tempdir().unwrap();
        let path = runs.path().join("trial/home");
        std::fs::create_dir_all(&path).unwrap();
        let identity = KeyIdentity::load_or_create(path.join("node.key"), None).unwrap();
        let at = locator(identity.did(), "trial");
        let missing = inspect_retained_requests(runs.path(), &at)
            .await
            .unwrap_err();
        assert!(
            format!("{missing:#}").contains("no existing database"),
            "{missing:#}"
        );
        assert!(!path.join("MANIFEST").exists());
        let node = crate::defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .with_regolith_options(crate::storage_backend::regolith_options())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap();
        node.shutdown().await;
        drop(node);
        for _ in 0..2 {
            let error = inspect_retained_requests(runs.path(), &at)
                .await
                .unwrap_err();
            let error = format!("{error:#}");
            assert!(error.contains("AgentRequest"), "{error}");
            assert!(!error.contains("already open"), "{error}");
        }
    }

    #[test]
    fn retained_requests_refuse_overflow_instead_of_truncating() {
        let response = json!({"data": {"AgentRequest": vec![json!({}); MAX_REQUESTS + 1]}});
        let error = decode_requests(response).unwrap_err();
        assert!(error.to_string().contains("more than 1000 requests"));
    }

    #[test]
    fn retained_paths_refuse_absolute_and_traversing_hints() {
        let runs = tempfile::tempdir().unwrap();
        for hint in ["", "../outside", "trial/../../outside", "/tmp/outside"] {
            assert!(retained_trial_dir(runs.path(), hint).is_err(), "{hint}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn retained_paths_refuse_symlinked_trial_home_and_workspace() {
        use std::os::unix::fs::symlink;
        for component in ["trial", "trial/home", "trial/workspace"] {
            let runs = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let path = runs.path().join(component);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            symlink(outside.path(), &path).unwrap();
            let error = retained_trial_dir(runs.path(), "trial").unwrap_err();
            assert!(error
                .to_string()
                .contains("outside the eval runs directory"));
        }
    }
}
