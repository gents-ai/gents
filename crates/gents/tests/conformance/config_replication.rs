//! Signed P2P replication fences canonical context/inference dependencies and excludes credentials.

use std::time::{Duration, Instant};

use gents::agent::p2p_reconcile::templates::single_string_eq;
use gents::agent::p2p_reconcile::{
    resolve_template, scope_filter, EmbeddedRemoteP2pAdmin, RemoteP2pAdmin,
};
use serde_json::Value;

use crate::support::test_p2p_db;

/// Peer principal the conversation grant is written for. Transcript rows
/// selected by the conversation grant requester filter.
const SAFE_CONFIG: &[&str] = &[
    "AgentBehavior",
    "AgentContext",
    "Tools",
    "CompactionConfig",
    "InferenceProfile",
    "InferenceSampling",
    "InferenceExecution",
    "InferenceRetryPolicy",
];

const PEER_DID: &str = "did:key:phone";
/// Foreign principal on the same source node: its AgentRequest row must stay
/// behind the replicator's requester-scoped AgentRequest filter.
const FOREIGN_DID: &str = "did:key:outsider";

#[tokio::test]
async fn signed_conversation_pairing_replays_agent_config_over_p2p() {
    let source = test_p2p_db("config-replication-source").await;
    let target = test_p2p_db("config-replication-target").await;
    let target_addr = wait_for_listen_addr(target.node.as_ref()).await;

    let response = source
        .node
        .execute(
            r#"mutation {
                create_AgentBehavior(input: {
                    behavior_id: "amy-default",
                    agent_did: "did:key:amy",
                    display_name: "Amy",
                    context_id: "amy-context",
                    inference_profile_id: "amy-profile",
                    enabled: true
                }) { _docID }
                create_InferenceBackend(input: {
                    backend_id: "amy-backend",
                    agent_did: "did:key:amy",
                    name: "Amy inference",
                    provider_kind: "OpenAiCompatible",
                    enabled: true
                }) { _docID }
                create_AgentContext(input: {
                    context_id: "amy-context", agent_did: "did:key:amy",
                    tools_id: "amy-tools", compaction_id: "amy-compaction"
                }) { _docID }
                create_Tools(input: { tools_id: "amy-tools", agent_did: "did:key:amy" }) { _docID }
                create_CompactionConfig(input: { compaction_id: "amy-compaction", agent_did: "did:key:amy" }) { _docID }
                create_InferenceProfile(input: {
                    profile_id: "amy-profile", agent_did: "did:key:amy",
                    backend_id: "amy-backend", model_name: "test-model",
                    sampling_id: "amy-sampling", execution_id: "amy-execution"
                }) { _docID }
                create_InferenceSampling(input: { sampling_id: "amy-sampling", agent_did: "did:key:amy" }) { _docID }
                create_InferenceExecution(input: { execution_id: "amy-execution", agent_did: "did:key:amy", retry_policy_id: "amy-retry" }) { _docID }
                create_InferenceRetryPolicy(input: { retry_policy_id: "amy-retry", agent_did: "did:key:amy" }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "seed config: {:?}", response.errors);

    // Seed the transcript plane the conversation template scopes by
    // requester_did: one row for the granted peer and one for a foreign
    // principal. Only the peer-owned row may cross the replicator.
    for (request_id, requester_did) in [
        ("req-phone-owned", PEER_DID),
        ("req-foreign-owned", FOREIGN_DID),
    ] {
        let request_id = gents::graphql::escape_graphql_string(request_id);
        let requester_did = gents::graphql::escape_graphql_string(requester_did);
        let response = source
            .node
            .execute(&format!(
                r#"mutation {{
                    create_AgentRequest(input: {{
                        request_id: "{request_id}",
                        agent_did: "did:key:amy",
                        requester_did: "{requester_did}",
                        behavior_id: "amy-default",
                        session_id: "{request_id}-session",
                        retry_parent_request: "",
                        retry_root_request: "{request_id}",
                        superseded_by_request: "",
                        content: "conversation replication seed",
                        lifecycle_state: "processing",
                        backend_id: "",
                        execution_origin: "interactive",
                        created_at: "2026-07-06T00:00:00Z",
                        retry_count: 0,
                        max_retries: 3,
                        subagent_depth: 0
                    }}) {{ _docID }}
                }}"#
            ))
            .await;
        assert!(
            !response.has_errors(),
            "seed AgentRequest {request_id}: {:?}",
            response.errors
        );
    }

    let template = resolve_template("conversation").expect("conversation template");
    let collections = template
        .collections
        .iter()
        .map(|collection| (*collection).to_string())
        .collect::<Vec<_>>();
    let filters = scope_filter(
        &template.scope,
        template.collections,
        PEER_DID,
        "did:key:amy",
    );
    for collection in SAFE_CONFIG {
        assert!(collections.iter().any(|value| value == *collection));
        assert!(
            !filters.contains_key(*collection),
            "config collection {collection} must cross the signed grant unfiltered"
        );
    }
    // The transcript plane the test actually seeds must be requester-scoped by
    // the owner template, otherwise the exclusion asserted below would pass
    // against an unfiltered transport too.
    let request_filter = filters
        .get("AgentRequest")
        .expect("conversation template scopes AgentRequest");
    assert_eq!(
        single_string_eq(request_filter),
        Some(("requester_did", PEER_DID)),
        "AgentRequest replication must be scoped to the granted requester"
    );

    EmbeddedRemoteP2pAdmin::new(source.node.clone())
        .add_replicator(&[target_addr], &collections, &filters)
        .await
        .expect("install P2P config replicator");

    let data = wait_for_config(target.node.as_ref(), Duration::from_secs(30)).await;
    for collection in SAFE_CONFIG {
        assert_eq!(row_count(&data, collection), 1, "missing {collection}");
    }
    assert!(!collections
        .iter()
        .any(|value| matches!(value.as_str(), "InferenceBackend" | "OAuthCredential")));
    assert_eq!(row_count(&data, "InferenceBackend"), 0);
    for (collection, field, expected) in [
        ("AgentBehavior", "context_id", "amy-context"),
        ("AgentBehavior", "inference_profile_id", "amy-profile"),
        ("AgentContext", "tools_id", "amy-tools"),
        ("AgentContext", "compaction_id", "amy-compaction"),
        ("InferenceProfile", "backend_id", "amy-backend"),
        ("InferenceProfile", "model_name", "test-model"),
        ("InferenceProfile", "sampling_id", "amy-sampling"),
        ("InferenceProfile", "execution_id", "amy-execution"),
        ("InferenceExecution", "retry_policy_id", "amy-retry"),
    ] {
        assert_eq!(
            data[collection][0][field].as_str(),
            Some(expected),
            "replication must preserve {collection}.{field}"
        );
    }

    // This is the converged snapshot observed below, not proof of transport
    // ordering or permanent absence; the filter owner is asserted separately.
    let requests = data
        .get("AgentRequest")
        .and_then(Value::as_array)
        .expect("AgentRequest rows in converged snapshot");
    assert_eq!(
        requests.len(),
        1,
        "the requester-scoped replicator must admit exactly the peer-owned request: {data}"
    );
    assert_eq!(
        requests[0].get("request_id").and_then(Value::as_str),
        Some("req-phone-owned")
    );
    assert_eq!(
        requests[0].get("requester_did").and_then(Value::as_str),
        Some(PEER_DID)
    );
}

async fn wait_for_listen_addr(node: &gents::defra_node::EmbeddedNode) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addresses = node
            .p2p()
            .expect("P2P enabled")
            .listen_addresses()
            .await
            .expect("listen addresses");
        if let Some(address) = addresses.first() {
            return address.clone();
        }
        assert!(Instant::now() < deadline, "P2P listen address timeout");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_config(node: &gents::defra_node::EmbeddedNode, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let response = node
            .execute(
                r#"query {
                    AgentBehavior(filter: { behavior_id: { _eq: "amy-default" } }) { behavior_id context_id inference_profile_id }
                    InferenceBackend(filter: { backend_id: { _eq: "amy-backend" } }) { backend_id }
                    InferenceProfile(filter: { profile_id: { _eq: "amy-profile" } }) { profile_id backend_id model_name sampling_id execution_id }
                    AgentContext { context_id tools_id compaction_id }
                    Tools { tools_id }
                    CompactionConfig { compaction_id }
                    InferenceSampling { sampling_id }
                    InferenceExecution { execution_id retry_policy_id }
                    InferenceRetryPolicy { retry_policy_id }
                    AgentRequest {
                        request_id
                        requester_did
                    }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "query replicated config: {:?}",
            response.errors
        );
        let data = response.data.unwrap_or(Value::Null);
        if SAFE_CONFIG
            .iter()
            .all(|collection| row_count(&data, collection) == 1)
            && row_count(&data, "AgentRequest") == 1
        {
            return data;
        }
        assert!(
            Instant::now() < deadline,
            "replicated config timeout; last={data}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn row_count(data: &Value, collection: &str) -> usize {
    data.get(collection)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}
