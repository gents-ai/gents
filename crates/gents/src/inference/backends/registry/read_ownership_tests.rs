use std::{sync::Arc, time::Duration};

use tokio::sync::Notify;

use super::lookup_backend_observation;
use crate::inference::llm::tool::Tool;
use crate::store::identity::NodeIdentity;
use crate::tools::builtin::defra_query::{CollectionScope, DefraQueryTool, QueryParams};
use crate::{defra_node::EmbeddedNode, store::config_client::ConfigAccess};

#[tokio::test]
async fn runtime_observation_reads_do_not_wait_for_the_mutation_gate() {
    const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(10);
    let keys = tempfile::tempdir().unwrap();
    let identity =
        crate::store::identity::KeyIdentity::load_or_create(keys.path().join("reader.key"), None)
            .unwrap();
    let owner = identity.did().to_owned();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(owner.clone())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let writer_node = Arc::clone(&node);
    let writer_entered = Arc::clone(&entered);
    let writer_release = Arc::clone(&release);
    let writer = tokio::spawn(async move {
        ConfigAccess::transact_local_write(
            &writer_node,
            None,
            "test.backend_observation.held_writer",
            |_| {
                let entered = Arc::clone(&writer_entered);
                let release = Arc::clone(&writer_release);
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok::<_, anyhow::Error>(())
                })
            },
        )
        .await
    });
    tokio::time::timeout(OBSERVATION_TIMEOUT, entered.notified())
        .await
        .expect("writer acquires the canonical mutation gate");

    let missing_doc = document::DocID::new_v0_from_seed("missing-observation").to_string();
    let query = DefraQueryTool::new(Arc::clone(&node), CollectionScope::all())
        .with_actor(::identity::Did::new(owner.clone()).unwrap());
    let query_args = QueryParams {
        argv: vec!["fields".into()],
        collection: Some("AgentRequest".into()),
        options: Default::default(),
    };
    let observations = tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(
            lookup_backend_observation(&node, &owner, "missing"),
            crate::session::max_sequence(&node, "missing", &owner, None),
            crate::session::load_session_current_hop(&node, &owner, "missing"),
            crate::tools::background::status::load_caused_request_terminal(&node, &missing_doc),
            crate::load_inference_profile(&node, &owner, "missing"),
            Tool::call(&query, query_args),
        )
    })
    .await;
    release.notify_one();
    writer.await.unwrap().unwrap();
    let (backend, sequence, hop, terminal, profile, query_result) =
        observations.expect("observation reads must not queue behind a mutation");
    assert!(backend.unwrap().is_none());
    assert_eq!(sequence.unwrap(), 0);
    assert_eq!(hop.unwrap(), 0);
    assert!(terminal.unwrap().is_none());
    assert!(profile.unwrap().is_none());
    let schema: serde_json::Value = serde_json::from_str(&query_result.unwrap()).unwrap();
    assert!(schema.is_object());
    node.shutdown().await;
}
