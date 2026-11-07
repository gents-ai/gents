use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::json;

use super::{apply_task_selections, selected_agent};
use crate::lean_vocab_test::lean_identity_household_cases;
use crate::store::config_client::desired_state::selection::tests::canonical;
use crate::store::config_client::desired_state::selection::{apply_selection_changes, Change};
use crate::store::config_client::ConfigAccess;
use crate::store::identity::NodeIdentity;
use crate::Collection;

#[test]
fn task_selection_edge_is_the_task_target_agent() -> Result<()> {
    let node = "did:key:node";
    assert_eq!(selected_agent(node, None)?, None);
    for agent in ["did:key:before", "did:key:after"] {
        let task = canonical(Collection::Task, node, json!({"agent_did": agent}));
        assert_eq!(selected_agent(node, Some(&task))?.as_deref(), Some(agent));
    }
    Ok(())
}

#[tokio::test]
async fn task_and_agent_target_visibility_follows_modeled_native_selection() -> Result<()> {
    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .context("configuration model")?;
    let dir = tempfile::tempdir()?;
    let key_dir = dir.path().to_owned();
    let (user, runtime, a, b, listener) = tokio::task::spawn_blocking(move || -> Result<_> {
        Ok((
            crate::KeyIdentity::load_or_create(key_dir.join("user"), None)?,
            crate::KeyIdentity::load_or_create(key_dir.join("runtime"), None)?,
            crate::KeyIdentity::load_or_create(key_dir.join("a"), None)?,
            crate::KeyIdentity::load_or_create(key_dir.join("b"), None)?,
            std::net::TcpListener::bind("127.0.0.1:0")?,
        ))
    })
    .await??;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let ctx = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&ctx, &model.policy_yaml, 0)
        .await?;
    for actor in [runtime.did(), a.did(), b.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&ctx, permission.as_str(), actor)
                .await?;
        }
    }
    let old_policy = gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID;
    // Configuration publication reads every configuration collection, so the
    // node declares them all, configuration under the modeled policy.
    crate::test_support::acp_policies::install_identity_acp_policies(&node).await?;
    for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
    {
        if gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.contains(name)
            || Collection::ALL
                .iter()
                .any(|collection| collection.graphql_type() == *name)
        {
            node.add_collection(&schema.replace(old_policy, &policy))
                .await?;
        }
    }
    // The User configures through client-signed access; a local non-node
    // write without a managed signer is refused by design.
    let access = ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            user.did(),
        )
        .with_client_signing_capability(user.client_signing_capability()?)?,
    );
    let esc = crate::store::graphql::escape_graphql_string;
    let create = |name: &str, fields: &str| {
        format!("mutation {{ create_{name}(input: {{{fields}}}) {{_docID}} }}")
    };
    let mut docs = Vec::new();
    for (name, fields) in [
        ("Node", format!("node_did: \"{}\"", esc(runtime.did()))),
        ("Agent", format!("node_did: \"{}\", agent_did: \"{}\", creation_nonce: \"a\", context_id: \"context\", inference_profile_id: \"profile\"", esc(runtime.did()), esc(a.did()))),
        ("Agent", format!("node_did: \"{}\", agent_did: \"{}\", creation_nonce: \"b\", inference_profile_id: \"profile\"", esc(runtime.did()), esc(b.did()))),
        ("AgentContext", format!("node_did: \"{}\", context_id: \"context\", tools_id: \"tools\"", esc(runtime.did()))),
        ("Tools", format!("node_did: \"{}\", tools_id: \"tools\", agents: {{enabled: false, target_ids: [\"target\"]}}", esc(runtime.did()))),
        ("AgentTarget", format!("node_did: \"{}\", target_id: \"target\", target_node_did: \"{}\", agent_did: \"{}\", name: \"target\"", esc(runtime.did()), esc(runtime.did()), esc(b.did()))),
        ("Task", format!("node_did: \"{}\", task_id: \"task\", agent_did: \"{}\", prompt_template: \"work\"", esc(runtime.did()), esc(a.did()))),
    ] {
        let result = access.write("test.task_target.create", &create(name, &fields)).await?;
        docs.push(crate::store::graphql::created_doc_id(&result, name)?);
    }
    let [node_doc, a_doc, b_doc, context_doc, tools_doc, target_doc, task_doc] = docs.as_slice()
    else {
        anyhow::bail!("fixture documents missing")
    };
    for (collection, doc) in [("Task", task_doc), ("AgentTarget", target_doc)] {
        access
            .add_resource_relationship(collection, doc, "parent", "nodes", node_doc)
            .await?;
    }
    access
        .add_actor_relationship(
            "Node",
            node_doc,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    for (doc, actor) in [(a_doc, a.did()), (b_doc, b.did())] {
        access
            .add_actor_relationship("Agent", doc, "executor", &identity::Did::new(actor)?)
            .await?;
    }
    let change = |collection, doc_id: &str, before: Option<serde_json::Value>, after| Change {
        collection,
        node_did: runtime.did().into(),
        doc_id: doc_id.into(),
        before: before.map(|before| canonical(collection, runtime.did(), before)),
        after: canonical(collection, runtime.did(), after),
    };
    let graph = vec![
        change(
            Collection::Agent,
            a_doc,
            None,
            json!({"context_id":"context"}),
        ),
        change(
            Collection::AgentContext,
            context_doc,
            None,
            json!({"tools_id":"tools"}),
        ),
        change(
            Collection::Tools,
            tools_doc,
            None,
            json!({"agents":{"enabled":false,"target_ids":["target"]}}),
        ),
    ];
    let first = vec![change(
        Collection::Task,
        task_doc,
        None,
        json!({"agent_did":a.did()}),
    )];
    access
        .transact_write("test.task_target.select", |txn| {
            let (graph, first) = (&graph, &first);
            Box::pin(async move {
                apply_selection_changes(txn, graph, |collection| {
                    collection != Collection::InferenceProfile
                })
                .await?;
                apply_task_selections(txn, first).await
            })
        })
        .await?;
    for case in &model.configuration_task_target_cases {
        let transition = case["transition"].as_str().context("transition")?;
        if case["name"] == "task-old-target" {
            let changes = vec![change(
                Collection::Task,
                task_doc,
                Some(json!({"agent_did":a.did()})),
                json!({"agent_did":b.did()}),
            )];
            let update = format!(
                "mutation {{ update_Task(docID: \"{}\", input: {{agent_did: \"{}\"}}) {{ _docID }} }}",
                esc(task_doc), esc(b.did())
            );
            access
                .transact_write("test.task_target.rebind", |txn| {
                    let (changes, update) = (&changes, &update);
                    Box::pin(async move {
                        txn.execute(update).await?;
                        apply_task_selections(txn, changes).await
                    })
                })
                .await?;
        }
        if case["name"] == "task-explicit-reader" {
            access
                .add_actor_relationship("Task", task_doc, "reader", &identity::Did::new(a.did())?)
                .await?;
        }
        if transition == "task-revoked" {
            let replay = vec![change(
                Collection::Task,
                task_doc,
                Some(json!({"agent_did":b.did()})),
                json!({"agent_did":b.did()}),
            )];
            access
                .transact_write("test.task_target.revoke_and_replay", |txn| {
                    let replay = &replay;
                    Box::pin(async move {
                        txn.delete_entity_set_relationship(
                            "Task",
                            task_doc,
                            "selected_by",
                            "agents",
                            b_doc,
                            "executor",
                        )
                        .await?;
                        apply_task_selections(txn, replay).await
                    })
                })
                .await?;
        }
        if transition == "target-revoked" {
            let changes = vec![change(
                Collection::Tools,
                tools_doc,
                Some(json!({"agents":{"target_ids":["target"]}})),
                json!({"agents":{"target_ids":[]}}),
            )];
            let update = format!(
                "mutation {{ update_Tools(docID: \"{}\", input: {{agents: {{enabled: false, target_ids: null}}}}) {{ _docID }} }}",
                esc(tools_doc)
            );
            access
                .transact_write("test.task_target.remove", |txn| {
                    let (changes, update) = (&changes, &update);
                    Box::pin(async move {
                        txn.execute(update).await?;
                        apply_selection_changes(txn, changes, |collection| {
                            collection != Collection::InferenceProfile
                        })
                        .await
                    })
                })
                .await?;
        }
        let actor = match case["actor"].as_str() {
            Some("user") => user.did(),
            Some("runtime") => runtime.did(),
            Some("agent-a") => a.did(),
            Some("agent-b") => b.did(),
            _ => anyhow::bail!("unknown model actor"),
        };
        let (name, doc) = if case["document"] == "task" {
            ("Task", task_doc)
        } else {
            ("AgentTarget", target_doc)
        };
        let response = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?)
            .execute(&format!("{{ {name} {{ _docID }} }}"))
            .await?;
        let visible = response["data"][name].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["_docID"].as_str() == Some(doc.as_str()))
        });
        assert_eq!(
            visible,
            case["expected"].as_bool().context("model expectation")?,
            "{}",
            case["name"]
        );
    }
    let modeled = |name: &str| -> Result<bool> {
        model
            .configuration_task_target_cases
            .iter()
            .find(|case| case["name"] == name)
            .and_then(|case| case["expected"].as_bool())
            .context("missing model case")
    };
    let signed_access = ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            user.did(),
        )
        .with_client_signing_capability(user.client_signing_capability()?)?
        .with_client_signed_agent_creates(),
    );
    for (actor, expected_case) in [(a.did(), "task-selected"), (b.did(), "task-unrelated")] {
        let value = json!({"node_did":runtime.did(), "task_id":"published-task", "agent_did":a.did(), "prompt_template":"work"});
        let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
            crate::store::config_client::DesiredStateApplyDocument {
                collection: Collection::Task,
                add: value.clone(),
                update: value,
            },
        ])?;
        // Publish once; repeating the same plan must preserve its native edge.
        signed_access
            .transact_write("test.task_target.apply", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::store::config_client::apply_desired_state_plan(txn, plan).await
                })
            })
            .await?;
        let agent_access = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?);
        let visible = crate::session::message::load_configured_task(
            &agent_access,
            runtime.did(),
            "published-task",
        )
        .await?
        .is_some();
        assert_eq!(visible, modeled(expected_case)?, "{expected_case}");
    }
    let published = signed_access
        .execute("{ Task(filter: {task_id: {_eq: \"published-task\"}}) {_docID} }")
        .await?;
    let published_doc = published["data"]["Task"][0]["_docID"]
        .as_str()
        .context("published Task missing")?
        .to_owned();
    let owners = access
        .transact_readonly("test.task_target.owner", |txn| {
            let published_doc = published_doc.clone();
            Box::pin(async move { txn.document_owners("Task", &[published_doc]).await })
        })
        .await?;
    assert_eq!(
        owners[0].owner.as_ref().map(|did| did.as_str()),
        Some(user.did())
    );
    let commits =
        crate::store::graphql::composite_commits(&node, &published_doc, "test.task_target.commits")
            .await?;
    let genesis = commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("Task genesis missing")?;
    let (block, signature) = node
        .authorized_signed_block_bytes(&genesis.cid, Some(user.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?,
        user.did()
    );
    let value = json!({"node_did":runtime.did(), "task_id":"published-task", "agent_did":b.did(), "prompt_template":"work"});
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: Collection::Task,
            add: value.clone(),
            update: value,
        },
    ])?;
    signed_access
        .transact_write("test.task_target.apply_rebind", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    for (actor, expected_case) in [(a.did(), "task-old-target"), (b.did(), "task-new-target")] {
        let agent_access = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?);
        let visible = crate::session::message::load_configured_task(
            &agent_access,
            runtime.did(),
            "published-task",
        )
        .await?
        .is_some();
        assert_eq!(visible, modeled(expected_case)?, "{expected_case}");
    }
    node.shutdown().await?;
    Ok(())
}
