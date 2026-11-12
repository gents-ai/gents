#![cfg(feature = "test-fixtures")]

use gents_documents::views::tasks::TaskList;
use gents_protocol::document_query::{fields::task, Create, Input};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use gents_store::views::ViewError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    OwnNode,
    CallingAgent,
}

fn task_id(same_node: bool, same_agent: bool, readable: bool) -> String {
    format!("view-{same_node}-{same_agent}-{readable}")
}

#[tokio::test]
async fn shared_view_runner_preserves_actor_and_scope_on_served_node() -> anyhow::Result<()> {
    let db = gents_inference::test_support::test_db(
        "shared-view-runner",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let owner = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did())?,
    );
    let reader = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("shared-view-reader")
    })
    .await?;
    let reader_did = identity::Did::new(reader.did())?;
    GrantSet::OperatorUser.grant(&owner, &reader_did).await?;
    for same_node in [false, true] {
        for same_agent in [false, true] {
            for readable in [false, true] {
                let id = task_id(same_node, same_agent, readable);
                let node = if same_node { "node" } else { "other-node" };
                let agent = if same_agent { reader.did() } else { "other-reader" };
                let create = Create::new(
                    Input::new()
                        .set(task::TASK_ID, id)
                        .set(task::NODE_DID, node)
                        .set(task::AGENT_DID, agent)
                        .set(task::PROMPT_TEMPLATE, "view fixture"),
                )
                .render();
                let response = owner.write_rendered("test.views.fixture", &create).await?;
                if readable {
                    let document = gents_store::graphql::created_doc_id(&response, "Task")?;
                    owner
                        .add_actor_relationship("Task", &document, "reader", &reader_did)
                        .await?;
                }
            }
        }
    }
    let http = db.client_access(&reader);
    let embedded = ConfigAccess::local_as(db.node.clone(), reader_did);
    let anonymous_http = ConfigAccess::graphql(db.client_endpoint());
    let anonymous_embedded = ConfigAccess::local(db.node.clone());
    let cases = gents_lean_vocab::families::read_views_runner::cases::<Scope>();
    assert_eq!(cases.len(), 32);
    for case in &cases {
        let input = &case.input;
        let id = task_id(input.same_node, input.same_agent, input.readable);
        let view = TaskList::new("node").task_id(id);
        let view = match input.scope {
            Scope::OwnNode => view,
            Scope::CallingAgent => view.calling_agent(),
        };
        let other_case = cases
            .iter()
            .find(|other| {
                other.input.scope == input.scope
                    && other.input.actor_present == input.actor_present
                    && other.input.same_node == input.same_node
                    && other.input.same_agent == input.same_agent
                    && other.input.readable != input.readable
            })
            .expect("modeled opposite readability");
        let other_view = TaskList::new("node").task_id(task_id(
            input.same_node,
            input.same_agent,
            !input.readable,
        ));
        let other_view = match input.scope {
            Scope::OwnNode => other_view,
            Scope::CallingAgent => other_view.calling_agent(),
        };
        for (access, anonymous) in [(&http, &anonymous_http), (&embedded, &anonymous_embedded)] {
            let access = if input.actor_present { access } else { anonymous };
            let result = access.read_view(&view).await;
            match result {
                Ok(rows) => case.check(gents_lean_vocab::families::read_views_runner::Decision {
                    admitted: true,
                    included: !rows.is_empty(),
                }),
                Err(ViewError::ActorRequired) => {
                    case.check(gents_lean_vocab::families::read_views_runner::Decision {
                        admitted: false,
                        included: false,
                    });
                }
                Err(error) => return Err(error.into()),
            }
            match access.read_views(&(&view, &other_view)).await {
                Ok((rows, other_rows)) => {
                    case.check(gents_lean_vocab::families::read_views_runner::Decision {
                        admitted: true,
                        included: !rows.is_empty(),
                    });
                    other_case.check(gents_lean_vocab::families::read_views_runner::Decision {
                        admitted: true,
                        included: !other_rows.is_empty(),
                    });
                }
                Err(ViewError::ActorRequired) => {
                    for expected in [case, other_case] {
                        expected.check(gents_lean_vocab::families::read_views_runner::Decision {
                            admitted: false,
                            included: false,
                        });
                    }
                }
                Err(error) => return Err(error.into()),
            }
            if input.actor_present {
                let rows = access
                    .transact_readonly("test.views.transaction", |txn| {
                        let view = &view;
                        Box::pin(async move { txn.read_view(view).await })
                    })
                    .await?;
                case.check(gents_lean_vocab::families::read_views_runner::Decision {
                    admitted: true,
                    included: !rows.is_empty(),
                });
                let (rows, other_rows) = access
                    .transact_readonly("test.views.transaction_tuple", |txn| {
                        let views = (&view, &other_view);
                        Box::pin(async move { txn.read_views(&views).await })
                    })
                    .await?;
                case.check(gents_lean_vocab::families::read_views_runner::Decision {
                    admitted: true,
                    included: !rows.is_empty(),
                });
                other_case.check(gents_lean_vocab::families::read_views_runner::Decision {
                    admitted: true,
                    included: !other_rows.is_empty(),
                });
            }
        }
    }
    db.node.shutdown().await?;
    Ok(())
}
