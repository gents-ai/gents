#![cfg(feature = "test-fixtures")]

use gents_inference::views::activity::InferenceCalls;
use gents_protocol::document_query::{fields::inference_call, Create, Input};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Active,
    Completed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Queued,
    Running,
    Cancelled,
    Completed,
    Failed,
}

#[tokio::test]
async fn inference_activity_views_match_model_on_served_node() -> anyhow::Result<()> {
    let db = gents_inference::test_support::test_db(
        "inference-activity-views",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let owner = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?);
    let reader = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("inference-activity-reader")
    })
    .await?;
    let reader_did = reader.did().parse()?;
    GrantSet::OperatorUser.grant(&owner, &reader_did).await?;
    let access = db.client_access(&reader);
    let cases =
        gents_lean_vocab::families::inference_activity_views_included::cases::<State, Mode>();
    assert!(!cases.is_empty());
    for (index, case) in cases.into_iter().enumerate() {
        let input = &case.input;
        let call_id = format!("activity-{index}");
        let state = serde_json::to_value(input.state)?;
        let create = Create::new(
            Input::new()
                .set(inference_call::CALL_ID, &call_id)
                .set(
                    inference_call::NODE_DID,
                    if input.same_node {
                        "node"
                    } else {
                        "other-node"
                    },
                )
                .set(inference_call::CALL_STATE, state.as_str().unwrap())
                .set(
                    inference_call::CALL_KIND,
                    if input.kind_eligible {
                        "inference"
                    } else {
                        "other"
                    },
                )
                .set(
                    inference_call::ENDED_AT,
                    if input.inside_window {
                        "2026-10-23T00:00:00Z"
                    } else {
                        "2026-10-21T00:00:00Z"
                    },
                ),
        )
        .render();
        let response = owner
            .write_rendered("test.activity.views.seed", &create)
            .await?;
        if input.readable {
            let doc_id = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
            owner
                .add_actor_relationship("InferenceCall", &doc_id, "reader", &reader_did)
                .await?;
        }
        let view = InferenceCalls::new("node").call_id(call_id);
        let view = match input.mode {
            Mode::Active => view.active(),
            Mode::Completed => view.completed_since("2026-10-22T00:00:00Z"),
        };
        let rows = access.read_view(&view).await?;
        case.check(!rows.is_empty());
    }
    db.node.shutdown().await?;
    Ok(())
}
