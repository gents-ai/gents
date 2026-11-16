use std::sync::Arc;

use gents_inference::inference_observation::{InferenceKey, InferenceSnapshot};

/// Lean `ClientInferenceObservation.ActivityResponse`.
#[derive(Debug)]
pub enum InferenceObservationRead {
    /// The caller holds no native node read.
    Forbidden,
    Unavailable,
    Visible(InferenceSnapshot),
}

impl super::Gents {
    /// Returns only a currently claimed request visible to the authenticated
    /// actor, after the route's native node read gate (Lean `activityResponse`).
    pub async fn read_inference_observation(
        &self,
        actor: identity::Did,
        key: &InferenceKey,
    ) -> anyhow::Result<InferenceObservationRead> {
        if key.node != self.node_did() {
            return Ok(InferenceObservationRead::Unavailable);
        }
        Ok(
            match observation_route(self.observation_node(), &actor, key).await? {
                ActivityRoute::Forbidden => InferenceObservationRead::Forbidden,
                ActivityRoute::Unavailable => InferenceObservationRead::Unavailable,
                ActivityRoute::Visible => self.inference_observations().snapshot(key).map_or(
                    InferenceObservationRead::Unavailable,
                    InferenceObservationRead::Visible,
                ),
            },
        )
    }
}

/// Lean `ClientInferenceObservation.ActivityResponse`, before the in-memory
/// snapshot is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActivityRoute {
    Forbidden,
    Unavailable,
    Visible,
}

/// Lean `activityResponse`: `None` when the actor holds no native node read,
/// otherwise the session and active request query it was allowed to run.
fn activity_route(response: Option<&serde_json::Value>) -> ActivityRoute {
    match response {
        None => ActivityRoute::Forbidden,
        Some(response) if activity_visible(response) => ActivityRoute::Visible,
        Some(_) => ActivityRoute::Unavailable,
    }
}

async fn observation_route(
    node: &Arc<defra_node::Node>,
    actor: &identity::Did,
    key: &InferenceKey,
) -> anyhow::Result<ActivityRoute> {
    use gents_protocol::document_query::{
        fields::{agent_request, agent_session},
        Filter, Query, QuerySet, Selection,
    };
    let sessions = Query::select(Selection::doc_id())
        .filter(Filter::eq(agent_session::NODE_DID, &key.node))
        .filter(Filter::eq(agent_session::SESSION_ID, &key.session))
        .limit(2);
    let requests = Query::select(Selection::doc_id().field(agent_request::LIFECYCLE_STATE))
        .filter(Filter::eq(agent_request::NODE_DID, &key.node))
        .filter(Filter::eq(agent_request::SESSION_ID, &key.session))
        .filter(Filter::eq(agent_request::REQUEST_ID, &key.request))
        .filter(Filter::eq(
            agent_request::EXECUTION_GENERATION,
            &key.generation,
        ))
        .limit(2);
    let query = QuerySet::new().plain(&sessions).plain(&requests).render();
    gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor.clone()),
        "inference_observation.authorize",
        |txn| {
            Box::pin(async {
                if !txn
                    .actor_holds_node_permission(
                        gents_store::config_client::NodePermission::DocumentRead,
                    )
                    .await?
                {
                    return Ok::<_, anyhow::Error>(activity_route(None));
                }
                let response = txn.run(&query).await?;
                Ok::<_, anyhow::Error>(activity_route(Some(&response)))
            })
        },
    )
    .await
}

fn activity_visible(response: &serde_json::Value) -> bool {
    let sessions = response
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array);
    let requests = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array);
    let (Some([session]), Some([request])) =
        (sessions.map(Vec::as_slice), requests.map(Vec::as_slice))
    else {
        return false;
    };
    if session
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .is_none()
        || request
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .is_none()
    {
        return false;
    }
    let state = request
        .get("lifecycle_state")
        .and_then(serde_json::Value::as_str);
    matches!(
        gents_protocol::request_lifecycle::RequestLifecycleState::parse_opt(state),
        Some(
            gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                | gents_protocol::request_lifecycle::RequestLifecycleState::Processing
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed_sources(sessions: u64, requests: u64, active: bool) -> serde_json::Value {
        let state = if active { "processing" } else { "completed" };
        serde_json::json!({"data": {
            "AgentSession": vec![serde_json::json!({"_docID":"session"}); sessions as usize],
            "AgentRequest": vec![serde_json::json!({"_docID":"request", "lifecycle_state":state}); requests as usize],
        }})
    }

    #[test]
    fn visibility_matches_lean_for_direct_reads() {
        gents_lean_vocab::families::inference_observation_source_visibility::replay(|case| {
            activity_visible(&observed_sources(case.sessions, case.requests, case.active))
        });
    }

    #[test]
    fn route_gate_matches_lean() {
        gents_lean_vocab::families::inference_observation_activity_route::replay::<ActivityRoute>(
            |case| {
                let response = observed_sources(case.sessions, case.requests, case.active);
                activity_route(case.holds_node_read.then_some(&response))
            },
        );
    }

    #[tokio::test]
    async fn delegated_reader_requires_both_document_permissions() {
        use gents_store::identity::NodeIdentity;
        let home = tempfile::tempdir().unwrap();
        let owner =
            gents_store::identity::KeyIdentity::load_or_create(home.path().join("owner"), None)
                .unwrap();
        let requester =
            gents_store::identity::KeyIdentity::load_or_create(home.path().join("requester"), None)
                .unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(owner.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        let policy = node
            .add_dac_policy_as(
                &defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned())),
                r#"
name: Activity read
resources:
  - name: documents
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
        let access = gents_store::config_client::ConfigAccess::local(node.clone());
        for schema in [
            format!("type AgentSession @branchable @policy(id: \"{policy}\", resource: \"documents\") {{ node_did: String session_id: String requester_did: String }}"),
            format!("type AgentRequest @branchable @policy(id: \"{policy}\", resource: \"documents\") {{ node_did: String session_id: String requester_did: String request_id: String execution_generation: String lifecycle_state: String }}"),
        ] { access.add_collection(&schema).await.unwrap(); }
        let actor: identity::Did = requester.did().parse().unwrap();
        let owner_did: identity::Did = owner.did().parse().unwrap();
        gents_store::config_client::ConfigAccess::local_as(node.clone(), owner_did.clone())
            .grant_node_permission(&actor, acp::nac::NodePermission::DocumentRead)
            .await
            .unwrap();
        let key = InferenceKey {
            node: "runtime".into(),
            session: "session".into(),
            request: "request".into(),
            generation: "generation".into(),
        };
        let requester_did = owner.did();
        let (session_doc, request_doc) = gents_store::config_client::ConfigAccess::transact_local_write(&node, Some(owner_did), "test.activity.acp", |txn| {
            Box::pin(async move {
                let session = txn.execute_with_variables("mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }", &serde_json::json!({"input":{"node_did":"runtime","session_id":"session","requester_did":requester_did}})).await?;
                let request = txn.execute_with_variables("mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }", &serde_json::json!({"input":{"node_did":"runtime","session_id":"session","requester_did":requester_did,"request_id":"request","execution_generation":"generation","lifecycle_state":"processing"}})).await?;
                Ok::<_, anyhow::Error>((gents_store::graphql::created_doc_id(&session, "AgentSession")?, gents_store::graphql::created_doc_id(&request, "AgentRequest")?))
            })
        }).await.unwrap();
        assert_eq!(
            observation_route(&node, &actor, &key).await.unwrap(),
            ActivityRoute::Unavailable
        );
        node.add_dac_actor_relationship(
            owner.did(),
            "AgentSession",
            &session_doc,
            "reader",
            requester.did(),
        )
        .await
        .unwrap();
        assert_eq!(
            observation_route(&node, &actor, &key).await.unwrap(),
            ActivityRoute::Unavailable
        );
        node.add_dac_actor_relationship(
            owner.did(),
            "AgentRequest",
            &request_doc,
            "reader",
            requester.did(),
        )
        .await
        .unwrap();
        assert_eq!(
            observation_route(&node, &actor, &key).await.unwrap(),
            ActivityRoute::Visible
        );
        // A revoked node read refuses the route even though both documents
        // stay visible to the actor (Lean `without_node_read_activity_is_forbidden`).
        gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            owner.did().parse().unwrap(),
        )
        .revoke_node_permission(&actor, acp::nac::NodePermission::DocumentRead)
        .await
        .unwrap();
        assert_eq!(
            observation_route(&node, &actor, &key).await.unwrap(),
            ActivityRoute::Forbidden
        );
        node.shutdown().await.expect("node shutdown");
    }
}
