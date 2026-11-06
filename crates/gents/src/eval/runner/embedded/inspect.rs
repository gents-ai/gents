use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use futures::FutureExt;
use gents_protocol::row::AgentRequestRow;

use super::EmbeddedHome;
use crate::eval::runner::TrialLocator;
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;

const MAX_REQUESTS: usize = 1_000;
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, serde::Serialize)]
pub struct RetainedRequest {
    pub request_doc_id: String,
    #[serde(flatten)]
    pub request: AgentRequestRow,
}

/// Reads request content and lineage as the held evaluator who created the trial.
/// No runtime or schema installer is started. The blocking task owns the home
/// until shutdown, even if the caller drops its future; a query panic also
/// unwinds only after shutdown.
pub async fn inspect_retained_requests(
    runtime: &tokio::runtime::Handle,
    runs_dir: &Path,
    locator: &TrialLocator,
    evaluator: &dyn crate::NodeIdentity,
) -> Result<Vec<RetainedRequest>> {
    let hint = locator
        .home_hint
        .as_deref()
        .context("trial has no retained home")?;
    let trial_dir = retained_trial_dir(runs_dir, hint)?;
    let home_dir = trial_dir.join("home");
    let did = locator.trial_node_did.clone();
    let signing = evaluator.client_signing_capability()?;
    ensure!(
        signing.did() == evaluator.did(),
        "held evaluator signing capability does not match its DID"
    );
    let evaluator_did = identity::Did::new(evaluator.did())?;
    crate::store::local::storage_backend::drive_unsendable(runtime, move || async move {
        let home = EmbeddedHome::open_for_inspection(&home_dir, &did)
            .await
            .with_context(|| {
                format!(
                    "opening retained trial home {} for inspection",
                    home_dir.display()
                )
            })?;
        let result = std::panic::AssertUnwindSafe(read_requests(&home, evaluator_did))
            .catch_unwind()
            .await;
        if let Err(error) = home.node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down trial node");
        }
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
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

async fn read_requests(
    home: &EmbeddedHome,
    evaluator: identity::Did,
) -> Result<Vec<RetainedRequest>> {
    let access = ConfigAccess::local_as(home.node.clone(), evaluator);
    let query = format!(
        r#"{{ AgentRequest(
            filter: {{ node_did: {{ _eq: "{}" }} }},
            order: {{ created_at: ASC }}, limit: {}
        ) {{
            _docID request_id purpose node_did requester_did agent_did session_id
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
    use crate::{KeyIdentity, NodeIdentity};

    fn locator(did: &str, hint: &str) -> TrialLocator {
        TrialLocator {
            trial_node_did: did.into(),
            session_id: "session".into(),
            home_hint: Some(hint.into()),
        }
    }

    async fn close(home: EmbeddedHome) {
        home.node.shutdown().await.expect("shutdown node");
    }

    #[tokio::test]
    async fn retained_requests_preserve_content_and_lineage_under_the_trial_identity() {
        use crate::request::lifecycle::{
            build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
            RequestSigner, RequestSpec, RetryLink,
        };
        use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
        let runs = tempfile::tempdir().unwrap();
        let path = runs.path().join("trial/home");
        let home = EmbeddedHome::create_retained(&path).await.unwrap();
        let db = crate::support::test_db_from_home(home);
        let (db, user, client, binding) =
            crate::test_support::signed_test_node_from_db(db, "eval-inspect-retained").await;
        let did = db.node_identity.did().to_owned();
        let content = "original \"prompt\"\nwith full content";
        let request_spec = |request_id: &str, body: &str, created_at: &str| {
            RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    request_id: request_id.to_owned(),
                    node_did: did.clone(),
                    requester_did: Some(user.did().to_owned()),
                    agent_did: binding.agent_did.clone(),
                    session_id: "session".to_owned(),
                    content: body.to_owned(),
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: created_at.to_owned(),
                },
                AgentRequestAdmissionRecord::client(user.did()),
            )
        };
        let root = build_signed_client_request(
            &client,
            request_spec("root", content, "2026-01-01T00:00:00Z"),
            RequestSigner::Identity(&user),
        )
        .await
        .unwrap();
        submit_client_request(&client, &root).await.unwrap();
        let response = client
            .execute("{ AgentRequest(filter: {request_id: {_eq: \"root\"}}) { _docID } }")
            .await
            .unwrap();
        let root_doc = response["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut child = request_spec("child", "child prompt", "2026-01-01T00:00:01Z");
        child.retry = Some(RetryLink {
            parent_request_id: Some("root".into()),
            parent_request_doc_id: Some(root_doc.clone()),
            root_request_id: "root".into(),
            retry_count: 1,
            max_retries: 3,
        });
        let child = build_signed_client_request(&client, child, RequestSigner::Identity(&user))
            .await
            .unwrap();
        submit_client_request(&client, &child).await.unwrap();
        let denied = KeyIdentity::load_or_create(runs.path().join("denied.key"), None).unwrap();
        let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(&did).unwrap());
        for permission in [
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&identity::Did::new(denied.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        let denied_access =
            ConfigAccess::local_as(db.node.clone(), identity::Did::new(denied.did()).unwrap());
        let query = format!(
            r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}}}) {{content}} }}"#,
            escape_graphql_string(&did)
        );
        let denied_rows = denied_access.execute(&query).await.unwrap();
        assert!(denied_rows["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        drop(denied_access);
        drop(admin);
        db.node.shutdown().await.expect("shutdown node");
        drop(db);
        let key = std::fs::read(path.join("node.key")).unwrap();
        let at = locator(&did, "trial");
        for _ in 0..2 {
            let requests = inspect_retained_requests(
                &crate::test_support::runtime_handle(),
                runs.path(),
                &at,
                &user,
            )
            .await
            .unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].request.content.as_deref(), Some(content));
            assert!(!requests[0].request_doc_id.is_empty());
            let child = &requests[1].request;
            assert_eq!(child.session_id.as_deref(), Some("session"));
            assert_eq!(child.retry_root_request.as_deref(), Some("root"));
            assert_eq!(child.retry_parent_request.as_deref(), Some("root"));
            assert_eq!(
                child.retry_parent_request_doc_id.as_deref(),
                Some(root_doc.as_str())
            );
            assert_eq!(std::fs::read(path.join("node.key")).unwrap(), key);
        }
    }

    #[test]
    fn retained_decoder_preserves_recorded_causal_and_runtime_lineage() {
        let rows = decode_requests(serde_json::json!({"data": {"AgentRequest": [{
            "_docID": "child-doc", "request_id": "child",
            "caused_by_parent_request_id": "root",
            "caused_by_parent_tool_call_id": "call",
            "caused_by_source_doc_id": "source",
            "runtime_source_request_id": "root",
            "runtime_source_kind": "session_message"
        }]}}))
        .unwrap();
        let child = &rows[0].request;
        assert_eq!(child.caused_by_parent_request_id.as_deref(), Some("root"));
        assert_eq!(child.caused_by_parent_tool_call_id.as_deref(), Some("call"));
        assert_eq!(child.caused_by_source_doc_id.as_deref(), Some("source"));
        assert_eq!(child.runtime_source_request_id.as_deref(), Some("root"));
        assert_eq!(
            child.runtime_source_kind.as_deref(),
            Some("session_message")
        );
    }

    #[tokio::test]
    async fn retained_requests_refuse_missing_mismatched_and_held_homes() {
        let runs = tempfile::tempdir().unwrap();
        let path = runs.path().join("trial/home");
        let evaluator =
            KeyIdentity::load_or_create(runs.path().join("evaluator.key"), None).unwrap();
        let home = EmbeddedHome::create_retained(&path).await.unwrap();
        let at = locator(home.did(), "trial");
        let admin =
            ConfigAccess::local_as(home.node.clone(), identity::Did::new(home.did()).unwrap());
        for permission in [
            crate::store::config_client::NodePermission::CollectionGet,
            crate::store::config_client::NodePermission::DocumentRead,
        ] {
            admin
                .grant_node_permission(&identity::Did::new(evaluator.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        drop(admin);
        let held = inspect_retained_requests(
            &crate::test_support::runtime_handle(),
            runs.path(),
            &at,
            &evaluator,
        )
        .await
        .unwrap_err();
        let held_text = format!("{held:#}");
        assert!(
            held_text.contains("already open in this process")
                || held_text.contains("already locked for read-write access")
                || (held_text.contains("another Gents runtime")
                    && held_text.contains("already using")
                    && held_text.contains(&path.display().to_string())),
            "{held:#}"
        );
        close(home).await;
        let mismatch = inspect_retained_requests(
            &crate::test_support::runtime_handle(),
            runs.path(),
            &locator("did:key:other", "trial"),
            &evaluator,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{mismatch:#}").contains("does not match recorded DID"),
            "{mismatch:#}"
        );
        assert!(inspect_retained_requests(
            &crate::test_support::runtime_handle(),
            runs.path(),
            &at,
            &evaluator
        )
        .await
        .unwrap()
        .is_empty());
        std::fs::remove_file(path.join("node.key")).unwrap();
        let missing = inspect_retained_requests(
            &crate::test_support::runtime_handle(),
            runs.path(),
            &at,
            &evaluator,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{missing:#}").contains("identity key does not exist"),
            "{missing:#}"
        );
        assert!(!path.join("node.key").exists());
        let missing = inspect_retained_requests(
            &crate::test_support::runtime_handle(),
            runs.path(),
            &locator(&at.trial_node_did, "missing"),
            &evaluator,
        )
        .await
        .unwrap_err();
        assert!(format!("{missing:#}").contains("is missing"), "{missing:#}");
        assert!(!runs.path().join("missing").exists());
    }

    #[tokio::test]
    async fn failed_inspection_releases_the_store_without_installing_schemas() {
        let runs = tempfile::tempdir().unwrap();
        let path = runs.path().join("trial/home");
        let evaluator =
            KeyIdentity::load_or_create(runs.path().join("evaluator.key"), None).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        let identity = KeyIdentity::load_or_create(path.join("node.key"), None).unwrap();
        let at = locator(identity.did(), "trial");
        let missing = inspect_retained_requests(
            &crate::test_support::runtime_handle(),
            runs.path(),
            &at,
            &evaluator,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{missing:#}").contains("no existing database"),
            "{missing:#}"
        );
        assert!(!path.join("MANIFEST").exists());
        let node = crate::defra_node::Node::builder()
            .data_path(&path)
            .with_regolith_options(crate::store::local::storage_backend::regolith_options())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap();
        node.shutdown().await.expect("shutdown node");
        drop(node);
        for _ in 0..2 {
            let error = inspect_retained_requests(
                &crate::test_support::runtime_handle(),
                runs.path(),
                &at,
                &evaluator,
            )
            .await
            .unwrap_err();
            let error = format!("{error:#}");
            assert!(error.contains("no existing database"), "{error}");
            assert!(!error.contains("already open"), "{error}");
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
