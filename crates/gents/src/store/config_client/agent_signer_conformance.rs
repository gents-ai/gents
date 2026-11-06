use std::sync::Arc;

use super::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity};

#[tokio::test]
async fn managed_signing_selection_conforms_to_lean() {
    let directory = tempfile::tempdir().unwrap();
    let root = KeyIdentity::load_or_create(directory.path().join("root"), None).unwrap();
    let foreign_root = KeyIdentity::load_or_create(directory.path().join("foreign"), None).unwrap();
    let user = KeyIdentity::load_or_create(directory.path().join("user"), None).unwrap();
    let binding_a = crate::store::identity::provision_agent_identity(&root).unwrap();
    let binding_b = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent_a = crate::store::identity::resolve_agent_identity(&root, &binding_a).unwrap();
    let agent_b = crate::store::identity::resolve_agent_identity(&root, &binding_b).unwrap();
    let foreign_binding_a =
        crate::store::identity::provision_agent_identity(&foreign_root).unwrap();
    let foreign_binding_b =
        crate::store::identity::provision_agent_identity(&foreign_root).unwrap();
    let foreign_agent_a =
        crate::store::identity::resolve_agent_identity(&foreign_root, &foreign_binding_a).unwrap();
    let foreign_agent_b =
        crate::store::identity::resolve_agent_identity(&foreign_root, &foreign_binding_b).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    let foreign_node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(foreign_root.did())
            .build()
            .await
            .unwrap(),
    );
    let unconfigured_node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .identity_agent_block_signing_cases
            .clone()
    })
    .await
    .unwrap();
    for case in &cases {
        let selected_node = match case.node.as_str() {
            "node" => &node,
            "foreign-node" => &foreign_node,
            "" => &unconfigured_node,
            other => panic!("unknown execution node {other}"),
        };
        let (binding_root, binding_a, binding_b, selected_a, selected_b): (
            &dyn NodeIdentity,
            _,
            _,
            &dyn NodeIdentity,
            &dyn NodeIdentity,
        ) = match case.binding_node.as_deref() {
            Some("foreign-node") => (
                &foreign_root,
                &foreign_binding_a,
                &foreign_binding_b,
                foreign_agent_a.as_ref(),
                foreign_agent_b.as_ref(),
            ),
            Some("node") | None => (
                &root,
                &binding_a,
                &binding_b,
                agent_a.as_ref(),
                agent_b.as_ref(),
            ),
            Some(other) => panic!("unknown binding node {other}"),
        };
        let actor = match case.actor.as_str() {
            "agent-a" => selected_a.did(),
            "agent-b" => selected_b.did(),
            "user" => user.did(),
            "node" => root.did(),
            other => other,
        };
        let actual = if !case.managed {
            let access = if actor.is_empty() {
                ConfigAccess::local(selected_node.clone())
            } else {
                ConfigAccess::local_as(selected_node.clone(), identity::Did::new(actor).unwrap())
            };
            assert!(!access.has_managed_agent_signer());
            if access.check_local_mutation_signer().is_ok() {
                "native"
            } else {
                "refuse"
            }
        } else {
            let selected = match case.signer.as_deref() {
                Some("agent-a") => Some((binding_a, selected_a)),
                Some("agent-b") => Some((binding_b, selected_b)),
                None => None,
                other => panic!("unknown managed signer {other:?}"),
            };
            let resolved = selected.and_then(|(binding, selected)| {
                let mut binding = binding.clone();
                if !case.valid {
                    binding.agent_did = user.did().to_owned();
                }
                if case.root_held {
                    crate::store::identity::resolve_agent_identity(binding_root, &binding).ok()
                } else {
                    let claimed = crate::store::identity::RegisteredIdentity::from_registered_did(
                        binding_root.did(),
                        None,
                    )
                    .unwrap();
                    let resolved =
                        crate::store::identity::resolve_agent_identity(&claimed, &binding);
                    assert!(
                        resolved.is_err(),
                        "{}: a DID-only root must not mint a managed signer",
                        case.name
                    );
                    resolved.ok()
                }
                .inspect(|identity| {
                    assert_eq!(identity.did(), selected.did());
                })
            });
            match identity::Did::new(actor).ok().and_then(|did| {
                resolved.as_ref().and_then(|identity| {
                    ConfigAccess::local_managed_agent(selected_node.clone(), did, identity.as_ref())
                        .ok()
                })
            }) {
                Some(access) => {
                    assert!(access.has_managed_agent_signer());
                    "agent"
                }
                None => "refuse",
            }
        };
        assert_eq!(actual, case.expected, "{}", case.name);
    }
    node.shutdown().await.unwrap();
    foreign_node.shutdown().await.unwrap();
    unconfigured_node.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_agent_signs_native_autocommit_and_transaction_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let root = KeyIdentity::load_or_create(directory.path().join("node"), None).unwrap();
    let user = KeyIdentity::load_or_create(directory.path().join("user"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    node.add_collection("type SigningFact { value: String }")
        .await
        .unwrap();
    let actor = identity::Did::new(agent.did()).unwrap();
    let unsigned_agent = ConfigAccess::local_as(node.clone(), actor.clone());
    assert!(matches!(
        unsigned_agent
            .transact_write("test.unsigned_agent_txn", |_txn| Box::pin(async {
                Ok::<_, super::ConfigClientError>(())
            }))
            .await,
        Err(super::ConfigClientError::ManagedAgentSignerRequired)
    ));
    let user_access = ConfigAccess::local_as(node.clone(), identity::Did::new(user.did()).unwrap());
    assert!(matches!(
        user_access
            .write(
                "test.unsigned_user",
                "mutation { add_SigningFact(input: {value: \"denied\"}) { _docID } }"
            )
            .await,
        Err(super::ConfigClientError::ManagedAgentSignerRequired)
    ));
    assert!(matches!(
        user_access
            .transact_write("test.unsigned_user_txn", |_txn| Box::pin(async {
                Ok::<_, super::ConfigClientError>(())
            }))
            .await,
        Err(super::ConfigClientError::ManagedAgentSignerRequired)
    ));
    user_access
        .transact_readonly("test.unsigned_user_read", |txn| {
            Box::pin(async move { txn.execute("{ SigningFact { _docID } }").await.map(|_| ()) })
        })
        .await
        .unwrap();
    let offline_client =
        ConfigAccess::local_as(node.clone(), identity::Did::new(user.did()).unwrap())
            .with_client_signed_configuration();
    assert!(matches!(
        offline_client
            .write(
                "test.offline_client",
                "mutation { add_SigningFact(input: {value: \"denied\"}) { _docID } }"
            )
            .await,
        Err(super::ConfigClientError::EmbeddedClientSigningUnavailable)
    ));
    let entered = std::sync::atomic::AtomicBool::new(false);
    assert!(matches!(
        offline_client
            .transact_write("test.offline_client_txn", |_txn| {
                entered.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok::<_, super::ConfigClientError>(()) })
            })
            .await,
        Err(super::ConfigClientError::EmbeddedClientSigningUnavailable)
    ));
    assert!(!entered.load(std::sync::atomic::Ordering::SeqCst));
    assert!(matches!(
        offline_client
            .add_actor_relationship(
                "SigningFact",
                "any-doc",
                "reader",
                &identity::Did::new(user.did()).unwrap(),
            )
            .await,
        Err(super::ConfigClientError::EmbeddedClientSigningUnavailable)
    ));
    offline_client
        .transact_readonly("test.offline_client_read", |txn| {
            Box::pin(async move { txn.execute("{ SigningFact { _docID } }").await.map(|_| ()) })
        })
        .await
        .unwrap();
    let runtime_as_client =
        ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap())
            .with_client_signed_configuration();
    assert!(matches!(
        runtime_as_client
            .write(
                "test.offline_client_same_did",
                "mutation { add_SigningFact(input: {value: \"denied\"}) { _docID } }"
            )
            .await,
        Err(super::ConfigClientError::EmbeddedClientSigningUnavailable)
    ));
    ConfigAccess::local(node.clone())
        .write(
            "test.default_node",
            "mutation { add_SigningFact(input: {value: \"node-default\"}) { _docID } }",
        )
        .await
        .unwrap();
    ConfigAccess::local(node.clone())
        .transact_write("test.default_node_txn", |txn| {
            Box::pin(async move {
                txn.execute(
                    "mutation { add_SigningFact(input: {value: \"default-txn\"}) { _docID } }",
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
    ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap())
        .write(
            "test.explicit_node",
            "mutation { add_SigningFact(input: {value: \"node-explicit\"}) { _docID } }",
        )
        .await
        .unwrap();
    ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap())
        .transact_write("test.explicit_node_txn", |txn| {
            Box::pin(async move {
                txn.execute("mutation { add_SigningFact(input: {value: \"node-txn\"}) { _docID } }")
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();
    let foreign_node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(user.did())
            .build()
            .await
            .unwrap(),
    );
    assert!(
        ConfigAccess::local_managed_agent(foreign_node.clone(), actor.clone(), agent.as_ref())
            .is_err()
    );
    foreign_node.shutdown().await.unwrap();
    let access =
        ConfigAccess::local_managed_agent(node.clone(), actor.clone(), agent.as_ref()).unwrap();
    assert!(ConfigAccess::local_managed_agent(node.clone(), actor.clone(), &user).is_err());
    assert!(ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(user.did()).unwrap(),
        agent.as_ref()
    )
    .is_err());
    let automatic = access
        .write(
            "test.managed_agent_autocommit",
            "mutation { add_SigningFact(input: {value: \"auto\"}) { _docID } }",
        )
        .await
        .unwrap();
    let automatic = automatic["data"]["add_SigningFact"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let transaction = access
        .transact_write("test.managed_agent_transaction", |txn| {
            Box::pin(async move {
                let value = txn
                    .execute(
                        "mutation { add_SigningFact(input: {value: \"transaction\"}) { _docID } }",
                    )
                    .await?;
                Ok::<_, super::ConfigClientError>(
                    value["data"]["add_SigningFact"][0]["_docID"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                )
            })
        })
        .await
        .unwrap();
    let expected = crate::store::identity::commit_signer_identity_for_did(agent.did()).unwrap();
    let node_signer = crate::store::identity::commit_signer_identity_for_did(root.did()).unwrap();
    for doc in [automatic, transaction] {
        let commits =
            crate::store::graphql::composite_commits(&node, &doc, "test managed agent signing")
                .await
                .unwrap();
        assert!(commits.iter().any(|commit| commit
            .signature
            .as_ref()
            .is_some_and(|signature| signature.identity == expected)));
        assert!(!commits.iter().any(|commit| commit
            .signature
            .as_ref()
            .is_some_and(|signature| signature.identity == node_signer)));
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn direct_lease_transaction_rejects_foreign_or_mismatched_signing_capability() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let directory = tempfile::tempdir().unwrap();
    let root = KeyIdentity::load_or_create(directory.path().join("root"), None).unwrap();
    let foreign = KeyIdentity::load_or_create(directory.path().join("foreign"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let other_binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let other = crate::store::identity::resolve_agent_identity(&root, &other_binding).unwrap();
    let signer = agent.managed_agent_signer().unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    let foreign_node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(foreign.did())
            .build()
            .await
            .unwrap(),
    );
    let entered = AtomicUsize::new(0);
    for (selected_node, actor) in [
        (
            &foreign_node,
            Some(identity::Did::new(agent.did()).unwrap()),
        ),
        (&node, Some(identity::Did::new(other.did()).unwrap())),
        (&node, None),
    ] {
        let result = ConfigAccess::renew_execution_lease_with_signer(
            selected_node,
            actor,
            Some(signer.clone()),
            |_txn| {
                entered.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok::<_, super::ConfigClientError>(()) })
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(super::ConfigClientError::TransactionActorMismatch)
        ));
    }
    assert_eq!(
        entered.load(Ordering::SeqCst),
        0,
        "rejected signer must not enter transaction callback"
    );
    ConfigAccess::renew_execution_lease_with_signer(
        &node,
        Some(identity::Did::new(agent.did()).unwrap()),
        Some(signer),
        |_txn| {
            entered.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok::<_, super::ConfigClientError>(()) })
        },
    )
    .await
    .unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    node.shutdown().await.unwrap();
    foreign_node.shutdown().await.unwrap();
}

#[tokio::test]
async fn direct_relationship_changes_require_actor_signing_capability() {
    let directory = tempfile::tempdir().unwrap();
    let root = KeyIdentity::load_or_create(directory.path().join("root"), None).unwrap();
    let user = KeyIdentity::load_or_create(directory.path().join("user"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&root).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    let target = identity::Did::new(user.did()).unwrap();
    for actor in [agent.did(), user.did()] {
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(actor).unwrap());
        for delete in [false, true] {
            let result = if delete {
                access
                    .delete_actor_relationship("SigningFact", "missing", "reader", &target)
                    .await
            } else {
                access
                    .add_actor_relationship("SigningFact", "missing", "reader", &target)
                    .await
            };
            assert!(matches!(
                result,
                Err(super::ConfigClientError::ManagedAgentSignerRequired)
            ));
        }
    }
    let context = defra_core::RequestContext::default().with_identity(Some(root.did().to_owned()));
    let policy = node.add_dac_policy_as(&context, "name: SigningFactPolicy\nresources:\n  - name: facts\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n").await.unwrap();
    node.add_collection(&format!(
        "type SigningFact @policy(id: \"{policy}\", resource: \"facts\") {{ value: String }}"
    ))
    .await
    .unwrap();
    let accesses = [
        ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap()),
        ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap(),
    ];
    for access in &accesses {
        let created = access
            .write(
                "test.relationship_owner",
                "mutation { add_SigningFact(input: {value: \"owned\"}) { _docID } }",
            )
            .await
            .unwrap();
        let doc = created["data"]["add_SigningFact"][0]["_docID"]
            .as_str()
            .unwrap();
        access
            .add_actor_relationship("SigningFact", doc, "reader", &target)
            .await
            .unwrap();
        let visible = ConfigAccess::local_as(node.clone(), target.clone())
            .execute("{ SigningFact { _docID } }")
            .await
            .unwrap();
        assert!(visible["data"]["SigningFact"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["_docID"] == doc));
        access
            .delete_actor_relationship("SigningFact", doc, "reader", &target)
            .await
            .unwrap();
        let hidden = ConfigAccess::local_as(node.clone(), target.clone())
            .execute("{ SigningFact { _docID } }")
            .await
            .unwrap();
        assert!(!hidden["data"]["SigningFact"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["_docID"] == doc));
    }
    node.shutdown().await.unwrap();
}
