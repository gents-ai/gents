use gents_documents::views::tasks::TaskList;
use gents_protocol::document_query::{fields::task, Create, Input};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn task_view_selection_matches_lean_on_served_node() -> anyhow::Result<()> {
    let db = gents_inference::test_support::test_db("task-views", Default::default()).await;
    let (owner, outsider) = tokio::task::spawn_blocking(|| {
        (
            gents_store::test_support::test_identity("task-view-owner"),
            gents_store::test_support::test_identity("task-view-outsider"),
        )
    })
    .await?;
    let access = |identity: &gents_store::identity::KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(db.client_endpoint(), identity.did())
                .with_client_signing_capability(identity.client_signing_capability()?)?,
        ))
    };
    let owner_access = access(&owner)?;
    let outsider_access = access(&outsider)?;
    for other_agent in [false, true] {
        let node = if other_agent {
            "node-other-agent"
        } else {
            "node-agent"
        };
        let agent = if other_agent {
            outsider.did()
        } else {
            owner.did()
        };
        for id in ["zeta", "alpha"] {
            let create = Create::new(
                Input::new()
                    .set(task::NODE_DID, node)
                    .set(task::TASK_ID, id)
                    .set(task::AGENT_DID, agent)
                    .set(task::PROMPT_TEMPLATE, "fixture")
                    .set(task::ENABLED, true),
            )
            .render();
            owner_access
                .write_rendered("test.task_views.create", &create)
                .await?;
        }
    }
    let cases = gents_lean_vocab::families::task_inventory_selection::cases();
    assert!(!cases.is_empty());
    for case in cases {
        let i = &case.input;
        let access = if i.reader_allowed {
            &owner_access
        } else {
            &outsider_access
        };
        let node = match (i.other_node, i.other_agent) {
            (true, _) => "absent-node",
            (false, true) => "node-other-agent",
            (false, false) => "node-agent",
        };
        let mut view = TaskList::new(node);
        if i.calling_agent {
            view = view.calling_agent();
        }
        if i.filter_task {
            view = view.task_id("zeta");
        }
        let rows = access.read_view(&view).await?;
        case.check(rows.into_iter().map(|row| row.task.task_id).collect());
    }
    Ok(())
}
