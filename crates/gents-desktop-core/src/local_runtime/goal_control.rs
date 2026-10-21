use anyhow::{ensure, Context, Result};
use gents_protocol::goal_control::{GOAL_STOP_CONTRACT_VERSION, STATUS_GOAL_STOP_CONTRACT_FIELD};
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use serde_json::Value;

use crate::client::PeerRecord;

use super::{http::peer_status, serving_runtime};

pub(crate) async fn ensure_goal_stop_supported(record: Option<&PeerRecord>) -> Result<()> {
    let Some(record) = record else {
        return Ok(());
    };
    let status = peer_status(record).await?.context(
        "Cannot verify goal assignment controls. Reconnect this runtime through its HTTP address.",
    )?;
    decode(&status, &record.node_did)
}

fn decode(status: &Value, expected_did: &str) -> Result<()> {
    ensure!(
        matches!(
            serving_runtime(status, expected_did)?,
            ObservedServeLifecycle::Ready
        ),
        "The runtime is not ready. Reconnect it before changing this goal."
    );
    ensure!(
        status
            .get(STATUS_GOAL_STOP_CONTRACT_FIELD)
            .and_then(Value::as_u64)
            == Some(u64::from(GOAL_STOP_CONTRACT_VERSION)),
        "The extension and runtime use different goal controls. Update them together, then reconnect."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn removal_requires_the_selected_ready_runtime_contract() {
        let node = "did:key:selected";
        let supported = json!({
            "node_did":node, "lifecycle":"ready",
            (STATUS_GOAL_STOP_CONTRACT_FIELD):GOAL_STOP_CONTRACT_VERSION,
        });
        decode(&supported, node).unwrap();
        assert!(decode(&supported, "did:key:other").is_err());
        for value in [
            Value::Null,
            json!(0),
            json!(GOAL_STOP_CONTRACT_VERSION + 1),
            json!("1"),
        ] {
            let mut status = supported.clone();
            status[STATUS_GOAL_STOP_CONTRACT_FIELD] = value;
            assert!(decode(&status, node).is_err());
        }
        let mut status = supported;
        status["lifecycle"] = json!("starting");
        assert!(decode(&status, node).is_err());
        status["lifecycle"] = json!("ready");
        status
            .as_object_mut()
            .unwrap()
            .remove(STATUS_GOAL_STOP_CONTRACT_FIELD);
        assert!(decode(&status, node).is_err());
    }

    #[tokio::test]
    async fn reads_support_from_the_saved_runtime_without_operator_credentials() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "node_did":"did:key:remote", "lifecycle":"ready",
                (STATUS_GOAL_STOP_CONTRACT_FIELD):GOAL_STOP_CONTRACT_VERSION,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut peer = PeerRecord::new("Remote", server.uri(), "did:key:remote");
        peer.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        ensure_goal_stop_supported(Some(&peer)).await.unwrap();
    }

    #[tokio::test]
    async fn a_replication_only_route_cannot_publish_an_unverified_stop_intent() {
        let peer = PeerRecord::new("Remote", "iroh://peer", "did:key:remote");
        let error = ensure_goal_stop_supported(Some(&peer)).await.unwrap_err();
        assert!(error.to_string().contains("HTTP address"));
    }

    #[tokio::test]
    async fn the_current_local_runtime_needs_no_remote_probe() {
        ensure_goal_stop_supported(None).await.unwrap();
    }
}
