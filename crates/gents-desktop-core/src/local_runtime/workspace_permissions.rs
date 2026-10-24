use gents::plugin::approval::workspace::{Support, CONTRACT_VERSION, STATUS_SUPPORT_FIELD};
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use serde_json::Value;

use crate::client::PeerRecord;

use super::{http::peer_status, serving_runtime, LocalRuntimeError, Result};

/// Capabilities belong to the process serving the saved peer, not the client
/// binary. Matching the node and lifecycle prevents a stale home or a different
/// service on the same port from supplying permission settings.
pub async fn workspace_permission_support(record: &PeerRecord) -> Result<Option<Support>> {
    let Some(status) = peer_status(record).await? else {
        return Ok(None);
    };
    decode(&status, &record.node_did)
}

fn decode(status: &Value, expected_did: &str) -> Result<Option<Support>> {
    match serving_runtime(status, expected_did)? {
        ObservedServeLifecycle::Ready => {}
        ObservedServeLifecycle::Starting => {
            return Err(LocalRuntimeError::WorkspaceRuntimeStarting)
        }
        ObservedServeLifecycle::Outdated { .. } => return Ok(None),
    }
    let Some(support) = status
        .get(STATUS_SUPPORT_FIELD)
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    if support
        .get("contractVersion")
        .and_then(Value::as_u64)
        .is_some_and(|version| version != u64::from(CONTRACT_VERSION))
    {
        return Ok(None);
    }
    let support: Support = serde_json::from_value(support.clone())
        .map_err(LocalRuntimeError::InvalidWorkspacePermissions)?;
    Ok(Some(support))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_capabilities_require_the_selected_ready_runtime() {
        let node = "did:key:selected";
        let mut status = json!({
            "node_did": node, "lifecycle": "ready",
            "workspace_permissions": {"contractVersion": CONTRACT_VERSION, "workspaceSandbox": false}
        });
        let support = decode(&status, node).unwrap().unwrap();
        assert!(!support.workspace_sandbox);
        assert_eq!(support.contract_version, CONTRACT_VERSION);
        assert!(decode(&status, "did:key:other").is_err());
        status["lifecycle"] = json!("starting");
        assert!(decode(&status, node).is_err());
        status["lifecycle"] = json!("ready");
        status[STATUS_SUPPORT_FIELD]["contractVersion"] = json!(CONTRACT_VERSION + 1);
        assert!(decode(&status, node).unwrap().is_none());
        status[STATUS_SUPPORT_FIELD]["futureCapability"] = json!(true);
        assert!(decode(&status, node).unwrap().is_none());
        status[STATUS_SUPPORT_FIELD] = json!({"workspaceSandbox": true});
        assert!(decode(&status, node).is_err());
        status.as_object_mut().unwrap().remove(STATUS_SUPPORT_FIELD);
        assert!(decode(&status, node).unwrap().is_none());
        status[STATUS_SUPPORT_FIELD] = Value::Null;
        assert!(decode(&status, node).unwrap().is_none());
        status.as_object_mut().unwrap().remove("lifecycle");
        assert!(decode(&status, node).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_replication_address_is_not_an_http_capability_endpoint() {
        let record = PeerRecord::new("Remote", "iroh://peer", "did:key:remote");
        assert!(workspace_permission_support(&record)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn permission_capabilities_are_read_from_the_saved_peer_without_credentials() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "node_did": "did:key:remote", "lifecycle": "ready",
                "workspace_permissions": {"contractVersion": CONTRACT_VERSION, "workspaceSandbox": false}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut record = PeerRecord::new("Remote", server.uri(), "did:key:remote");
        record.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        assert!(
            !workspace_permission_support(&record)
                .await
                .unwrap()
                .unwrap()
                .workspace_sandbox
        );
        let requests = server.received_requests().await.unwrap();
        assert!(!requests[0].headers.contains_key("authorization"));
        assert!(requests[0].body.is_empty());
    }
}
