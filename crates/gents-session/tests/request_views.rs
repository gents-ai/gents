use gents_session::views::requests::{CausedRequests, RequestRows};
use gents_session::views::transcript::Scope;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use serde_json::json;

#[tokio::test]
async fn request_views_match_model_on_served_node() -> anyhow::Result<()> {
    let cases = tokio::task::spawn_blocking(
        gents_lean_vocab::families::request_views_observed_seed::cases::<String>,
    )
    .await?;
    assert_eq!(cases.len(), 192);
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
    let mut documents = Vec::new();
    for (retry_parent, retry_parent_document) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let index = documents.len();
        let node = format!("did:example:view-node-{index}");
        let mut create = json!({
            "request_id": format!("view-request-{index}"), "node_did": node,
            "session_id": "view-session", "purpose": "normal"
        });
        if retry_parent {
            create["retry_parent_request"] = json!("parent");
        }
        if retry_parent_document {
            create["retry_parent_request_doc_id"] = json!("parent-document");
        }
        let variables = json!({ "input": create });
        let response = owner
            .transact_write("test.request_views.create", |txn| {
                let variables = &variables;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        variables,
                        gents_protocol::schemas::AGENT_REQUEST,
                    ).await
                })
            })
            .await?;
        let document = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
        owner
            .add_actor_relationship("AgentRequest", &document, "reader", &reader.did().parse()?)
            .await?;
        documents.push((node, document));
    }
    for case in cases {
        let input = &case.input;
        let (node, document) = &documents
            [usize::from(input.retry_parent) * 2 + usize::from(input.retry_parent_document)];
        let state = match input.state.as_str() {
            "absent" => None,
            "pending" => Some("pending"),
            "completed" => Some("completed"),
            other => anyhow::bail!("unknown modeled request state {other}"),
        };
        let mutation = format!(
            r#"mutation($input: AgentRequestMutationInputArg!) {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: $input) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(document)
        );
        let variables = json!({"input": {
            "lifecycle_state": state,
            "superseded_by_request_doc_id": input.successor.then_some("successor-document"),
        }});
        owner
            .transact_write("test.request_views.update", |txn| {
                let (mutation, variables) = (&mutation, &variables);
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        mutation,
                        variables,
                        gents_protocol::schemas::AGENT_REQUEST,
                    )
                    .await
                })
            })
            .await?;
        let access = db.client_access(if input.readable { &reader } else { &stranger });
        let view = RequestRows::new(Scope::Node(if input.same_node {
            node.clone()
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
    let cases =
        tokio::task::spawn_blocking(gents_lean_vocab::families::request_views_causal_child::cases)
            .await?;
    assert_eq!(cases.len(), 16);
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
    let mut documents = Vec::new();
    for (public, has_tool) in [(false, false), (false, true), (true, false), (true, true)] {
        let index = documents.len();
        let parent = format!("read-parent-{index}");
        let purpose = if public {
            gents_protocol::request_admission::RequestPurpose::Normal
        } else {
            gents_protocol::request_admission::RequestPurpose::TitleAudit
        };
        let mut create = json!({
            "request_id": format!("view-child-{index}"), "node_did": "did:example:foreign-node",
            "session_id": "child-session", "purpose": purpose.as_str(),
            "caused_by_parent_request_doc_id": parent
        });
        if has_tool {
            create["caused_by_parent_tool_call_doc_id"] = json!("parent-tool");
        }
        let variables = json!({ "input": create });
        let response = owner
            .transact_write("test.causal_request_views.create", |txn| {
                let variables = &variables;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        variables,
                        gents_protocol::schemas::AGENT_REQUEST,
                    ).await
                })
            })
            .await?;
        let document = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
        owner
            .add_actor_relationship("AgentRequest", &document, "reader", &reader.did().parse()?)
            .await?;
        documents.push(parent);
    }
    for case in cases {
        let input = &case.input;
        let parent = &documents[usize::from(input.public) * 2 + usize::from(input.has_tool)];
        let access = db.client_access(if input.readable { &reader } else { &stranger });
        let view = CausedRequests::new(vec![if input.anchored {
            parent.clone()
        } else {
            "other-parent".to_owned()
        }]);
        let rows = access.read_view(&view).await?;
        case.check(!rows.is_empty());
    }
    db.node.shutdown().await?;
    Ok(())
}
