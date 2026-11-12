use gents_protocol::operations::session::SessionListInput;
use gents_session::views::{SessionCount, SessionList, SessionPage};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn session_views_match_model_on_served_node() -> anyhow::Result<()> {
    let (db, user, _, binding) = gents_inference::test_support::signed_test_node(
        "session-views",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let agent = gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)?;
    let agent_access =
        ConfigAccess::local_managed_agent(db.node.clone(), agent.did().parse()?, agent.as_ref())?;
    let session_id = "view-visible-session";
    gents_session::test_support::create_agent_owned_session(
        &agent_access,
        db.node_identity.did(),
        session_id,
        Some(user.did()),
    )
    .await;
    let key_path = db.data_path().join("view-stranger.key");
    let stranger = tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_or_create(key_path, None)
    })
    .await??;
    gents_store::identity::GrantSet::OperatorUser
        .grant(
            &ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?),
            &stranger.did().parse()?,
        )
        .await?;
    let cases = gents_lean_vocab::families::session_views_visible::cases();
    assert_eq!(cases.len(), 32);
    let mut archived = false;
    let mut revision = 0;
    for case in cases {
        let input = &case.input;
        if input.archived != archived {
            let receipt = gents_session::session::set_session_archived_on(
                &agent_access,
                db.node_identity.did(),
                session_id,
                agent.as_ref(),
                revision,
                input.archived,
            )
            .await?;
            revision = receipt.revision.parse()?;
            archived = input.archived;
        }
        let access = db.client_access(if input.readable { &user } else { &stranger });
        let view = SessionList {
            input: SessionListInput {
                node_did: Some(if input.same_node {
                    db.node_identity.did().to_owned()
                } else {
                    "did:example:other-node".to_owned()
                }),
                requester_did: Some(if input.same_requester {
                    user.did().to_owned()
                } else {
                    stranger.did().to_owned()
                }),
                session_id: Some(session_id.to_owned()),
                include_archived: input.include_archived,
            },
        };
        let rows = access.read_view(&view).await?;
        let count = access
            .read_view(&SessionCount {
                page: SessionPage {
                    input: view.input,
                    ..Default::default()
                },
            })
            .await?;
        case.check(!rows.is_empty());
        case.check(count.into_iter().sum::<u64>() > 0);
    }
    let other_session = "view-other-requester";
    gents_session::test_support::create_agent_owned_session(
        &agent_access,
        db.node_identity.did(),
        other_session,
        Some(stranger.did()),
    )
    .await;
    let caller_cases = gents_lean_vocab::families::session_views_caller_visible::cases();
    assert_eq!(caller_cases.len(), 4);
    for case in caller_cases {
        let input = &case.input;
        let same_user = input.readable == input.same_requester;
        let selected = if same_user { session_id } else { other_session };
        let access = db.client_access(if input.readable { &user } else { &stranger });
        let view = SessionList {
            input: SessionListInput {
                session_id: Some(selected.to_owned()),
                include_archived: true,
                ..Default::default()
            },
        };
        let rows = access.read_view(&view).await?;
        case.check(!rows.is_empty());
    }
    db.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn session_ordering_matches_model_on_served_node() -> anyhow::Result<()> {
    use gents_protocol::document_query::{fields::agent_session as session, Input, Update};
    let cases = gents_lean_vocab::families::session_ordering::cases();
    assert_eq!(cases.len(), 6);
    for (index, case) in cases.into_iter().enumerate() {
        let (db, user, _, binding) = gents_inference::test_support::signed_test_node(
            &format!("session-ordering-{index}"),
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)?;
        let access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent.did().parse()?,
            agent.as_ref(),
        )?;
        for source in &case.input {
            let doc_id = gents_session::test_support::create_agent_owned_session(
                &access,
                db.node_identity.did(),
                &source.session_id,
                Some(user.did()),
            )
            .await;
            let mutation = Update::doc(
                doc_id,
                Input::new()
                    .set(session::CREATED_AT, source.created_at.as_str())
                    .set(
                        session::OBSERVATION,
                        serde_json::json!({"last_activity_at": source.last_activity_at}),
                    ),
            )
            .render();
            access
                .transact_write("test.session_view_order", |txn| {
                    let mutation = &mutation;
                    Box::pin(async move { txn.run(mutation).await })
                })
                .await?;
        }
        let listed = db
            .client_access(&user)
            .read_view(&SessionList {
                input: SessionListInput {
                    node_did: Some(db.node_identity.did().to_owned()),
                    requester_did: Some(user.did().to_owned()),
                    ..Default::default()
                },
            })
            .await?;
        case.check(
            listed
                .into_iter()
                .map(|row| row.session.session_id)
                .collect(),
        );
        db.node.shutdown().await?;
    }
    Ok(())
}
