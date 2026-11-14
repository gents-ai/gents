#![cfg(feature = "test-fixtures")]

use anyhow::{Context, Result};
use gents_inference::views::context::{ContextCalls, ContextCursor};
use gents_protocol::document_query::{fields::inference_call, Create, Input};
use gents_store::config_client::ConfigAccess;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Cursor {
    Start,
    FirstNull,
    SecondNull,
    Early,
    Queued,
    Sequenced,
    Late,
}

#[tokio::test]
async fn context_view_nullable_keyset_matches_model_on_served_node() -> Result<()> {
    let cases = gents_lean_vocab::families::inference_context_views_page::cases::<Cursor>();
    anyhow::ensure!(!cases.is_empty(), "Lean emitted no context view cases");
    let db = gents_inference::test_support::test_db(
        "inference-context-views",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let access = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?);
    let mut documents = Vec::new();
    for (index, (queued, sequence, call)) in [
        (None, None, None),
        (None, None, None),
        (Some("a"), None, None),
        (Some("b"), None, None),
        (Some("b"), Some(1_i64), None),
        (Some("b"), Some(1_i64), Some("b")),
    ]
    .into_iter()
    .enumerate()
    {
        let create = Create::new(
            Input::new()
                .set(inference_call::NODE_DID, "node")
                .set(inference_call::SESSION_ID, "session")
                .set(inference_call::CALL_KIND, "inference")
                .set(inference_call::REQUEST_ID, format!("request-{index}"))
                .set_optional(inference_call::QUEUED_AT, queued)
                .set_optional(inference_call::CALL_SEQ, sequence)
                .set_optional(inference_call::CALL_ID, call),
        )
        .render();
        let result = access
            .write_rendered("test.context.views.seed", &create)
            .await?;
        documents.push(gents_store::graphql::created_doc_id(
            &result,
            "InferenceCall",
        )?);
    }
    documents[..2].sort();
    let rows = access
        .read_view(&ContextCalls::indexed("node", "session"))
        .await?;
    let reader = db.client_access(db.node_identity.as_ref());
    for case in cases {
        let label = match case.input.cursor {
            Cursor::Start => None,
            Cursor::FirstNull => Some(0),
            Cursor::SecondNull => Some(1),
            Cursor::Early => Some(2),
            Cursor::Queued => Some(3),
            Cursor::Sequenced => Some(4),
            Cursor::Late => Some(5),
        };
        let mut view = ContextCalls::indexed("node", "session").limit(2);
        if let Some(label) = label {
            let cursor = rows
                .iter()
                .find(|row| row.doc_id == documents[label])
                .context("cursor fixture missing")?;
            view = view.after(ContextCursor::from(cursor));
        }
        let mut page = reader.read_view(&view).await?;
        page.reverse();
        gents_inference::views::context::retain_latest(&mut page, 2);
        let labels = page
            .iter()
            .map(|row| {
                documents
                    .iter()
                    .position(|id| id == &row.doc_id)
                    .map(|label| label as u64)
                    .context("unknown context row")
            })
            .collect::<Result<Vec<_>>>()?;
        case.check(labels);
    }
    db.node.shutdown().await?;
    Ok(())
}
