use gents_protocol::document_query::{fields::agent_request as request, Input, Update};
use gents_session::views::requests::{CausedRequests, RequestRows};
use gents_session::views::transcript::Scope;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use serde_json::json;

#[tokio::test]
async fn request_views_match_model_on_served_node() -> anyhow::Result<()> {
    let (db, _, owner) = gents_inference::test_support::signed_test_node_without_agent(
        "request-views",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let reader = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("request-view-reader")
    })
    .await?;
    let stranger = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("request-view-stranger")
    })
    .await?;
    let administrator = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?);
    for actor in [&reader, &stranger] {
        GrantSet::OperatorUser
            .grant(&administrator, &actor.did().parse()?)
            .await?;
    }
    let response = owner
        .transact_write("test.request_views.create", |txn| {
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": {
                        "request_id": "view-request", "node_did": "did:example:view-node",
                        "session_id": "view-session", "purpose": "normal"
                    }}),
                    gents_protocol::schemas::AGENT_REQUEST,
                ).await
            })
        })
        .await?;
    let document = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
    owner
        .add_actor_relationship("AgentRequest", &document, "reader", &reader.did().parse()?)
        .await?;
    let cases = gents_lean_vocab::families::request_views_observed_seed::cases::<String>();
    assert_eq!(cases.len(), 192);
    for case in cases {
        let input = &case.input;
        let state = match input.state.as_str() {
            "absent" => None,
            "pending" => Some("pending"),
            "completed" => Some("completed"),
            other => anyhow::bail!("unknown modeled request state {other}"),
        };
        let update = Update::doc(
            &document,
            Input::new()
                .set_optional(request::LIFECYCLE_STATE, state)
                .set_optional(
                    request::RETRY_PARENT_REQUEST,
                    input.retry_parent.then_some("parent"),
                )
                .set_optional(
                    request::RETRY_PARENT_REQUEST_DOC_ID,
                    input.retry_parent_document.then_some("parent-document"),
                )
                .set_optional(
                    request::SUPERSEDED_BY_REQUEST_DOC_ID,
                    input.successor.then_some("successor-document"),
                ),
        )
        .render();
        owner
            .transact_write("test.request_views.update", |txn| {
                let update = &update;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        &update.document,
                        &update.variables,
                        gents_protocol::schemas::AGENT_REQUEST,
                    )
                    .await
                })
            })
            .await?;
        let access = db.client_access(if input.readable { &reader } else { &stranger });
        let view = RequestRows::new(Scope::Node(if input.same_node {
            "did:example:view-node".to_owned()
        } else {
            "did:example:other-node".to_owned()
        }))
        .public()
        .seeds(if input.on_page {
            vec![document.clone()]
        } else {
            Vec::new()
        });
        case.check(!access.read_view(&view).await?.is_empty());
    }
    db.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn causal_request_views_match_model_on_served_node() -> anyhow::Result<()> {
    let (db, _, owner) = gents_inference::test_support::signed_test_node_without_agent(
        "causal-request-views",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let (reader, stranger) = tokio::task::spawn_blocking(|| {
        (
            gents_store::test_support::test_identity("causal-request-reader"),
            gents_store::test_support::test_identity("causal-request-stranger"),
        )
    })
    .await?;
    let administrator = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?);
    for actor in [&reader, &stranger] {
        GrantSet::OperatorUser
            .grant(&administrator, &actor.did().parse()?)
            .await?;
    }
    let response = owner
        .transact_write("test.causal_request_views.create", |txn| {
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": {
                        "request_id": "view-child", "node_did": "did:example:foreign-node",
                        "session_id": "child-session", "purpose": "normal",
                        "caused_by_parent_request_doc_id": "read-parent"
                    }}),
                    gents_protocol::schemas::AGENT_REQUEST,
                ).await
            })
        })
        .await?;
    let document = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
    owner
        .add_actor_relationship("AgentRequest", &document, "reader", &reader.did().parse()?)
        .await?;
    let cases = gents_lean_vocab::families::request_views_causal_child::cases();
    assert_eq!(cases.len(), 16);
    for case in cases {
        let input = &case.input;
        let purpose = if input.public {
            gents_protocol::request_admission::RequestPurpose::Normal
        } else {
            gents_protocol::request_admission::RequestPurpose::TitleAudit
        };
        let update = Update::doc(
            &document,
            Input::new()
                .set(request::PURPOSE, purpose.as_str())
                .set_optional(
                    request::CAUSED_BY_PARENT_TOOL_CALL_DOC_ID,
                    input.has_tool.then_some("parent-tool"),
                ),
        )
        .render();
        owner
            .transact_write("test.causal_request_views.update", |txn| {
                let update = &update;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        &update.document,
                        &update.variables,
                        gents_protocol::schemas::AGENT_REQUEST,
                    )
                    .await
                })
            })
            .await?;
        let access = db.client_access(if input.readable { &reader } else { &stranger });
        let view = CausedRequests::new(vec![if input.anchored {
            "read-parent".to_owned()
        } else {
            "other-parent".to_owned()
        }]);
        let rows = access.read_view(&view).await?;
        case.check(!rows.is_empty());
    }
    db.node.shutdown().await?;
    Ok(())
}
