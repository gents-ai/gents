use super::*;

fn grants() -> [ConfigRelationshipGrant; 3] {
    [
        ConfigRelationshipGrant::Resource {
            collection: "Agent".into(),
            doc_id: "agent-doc".into(),
            relation: "parent".into(),
            target_resource: "nodes".into(),
            target_doc_id: "node-doc".into(),
        },
        ConfigRelationshipGrant::Actor {
            collection: "Agent".into(),
            doc_id: "agent-doc".into(),
            relation: "executor".into(),
            target: "agent".into(),
        },
        ConfigRelationshipGrant::Actor {
            collection: "Agent".into(),
            doc_id: "agent-doc".into(),
            relation: "host".into(),
            target: "node".into(),
        },
    ]
}

async fn client_node() -> Result<(Arc<defra_node::Node>, String)> {
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    crate::schema::ensure_runtime_schemas(&node).await?;
    let client = node
        .node_identity_did()
        .context("client identity")?
        .to_owned();
    Ok((node, client))
}

/// Drives the observer the way the configuration transaction does, up to
/// and including `before_grant` of `stop_before_granted`, then drops it as
/// a crash would.
async fn interrupted(
    node: &Arc<defra_node::Node>,
    client: &str,
    stop_before_granted: Option<PublicationStage>,
) -> Result<()> {
    let observer = Observer::new(
        node.clone(),
        client.to_owned(),
        vec![("agent".into(), "node".into())],
    );
    let grants = grants();
    observer.before_commit(client, &grants, &[]).await?;
    observer.committed().await?;
    for grant in &grants {
        let stage = grant_stage(grant).context("journaled grant")?;
        observer.before_grant(grant).await?;
        if stop_before_granted == Some(stage) {
            return Ok(());
        }
        observer.granted(grant).await?;
        if stop_before_granted.is_none() && stage == PublicationStage::Executor {
            return Ok(());
        }
    }
    Ok(())
}

async fn recovered(node: &Arc<defra_node::Node>, client: &str) -> Result<()> {
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(client)?);
    ensure_recovered(node, &access, client, "node", "agent").await
}

#[tokio::test]
async fn crash_after_the_executor_grant_leaves_host_unfinished() -> Result<()> {
    let (node, client) = client_node().await?;
    interrupted(&node, &client, None).await?;
    let retained = lookup(&node, &client, "node", "agent")
        .await?
        .context("retained record")?;
    assert_eq!(retained.journal.executor, PublicationStatus::Succeeded);
    assert_eq!(retained.journal.host, PublicationStatus::Fresh);
    assert!(!retained.terminal());
    assert!(!retained.journal.reads_host(false));
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn crash_after_the_host_request_leaves_host_pending() -> Result<()> {
    let (node, client) = client_node().await?;
    interrupted(&node, &client, Some(PublicationStage::Host)).await?;
    let retained = lookup(&node, &client, "node", "agent")
        .await?
        .context("retained record")?;
    assert_eq!(retained.journal.host, PublicationStatus::Pending);
    assert!(!retained.terminal());
    assert!(retained.journal.reads_host(false));
    assert!(!retained.journal.reads_host(true));
    let again = Observer::new(
        node.clone(),
        client.clone(),
        vec![("agent".into(), "node".into())],
    );
    assert!(
        again.before_commit(&client, &grants(), &[]).await.is_err(),
        "an interrupted publication is not replaced"
    );
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn publication_is_terminal_only_after_the_host_grant() -> Result<()> {
    let (node, client) = client_node().await?;
    let observer = Observer::new(
        node.clone(),
        client.clone(),
        vec![("agent".into(), "node".into())],
    );
    let grants = grants();
    observer.before_commit(&client, &grants, &[]).await?;
    observer.committed().await?;
    for grant in &grants {
        observer.before_grant(grant).await?;
        observer.granted(grant).await?;
        let retained = lookup(&node, &client, "node", "agent")
            .await?
            .context("retained record")?;
        assert_eq!(
            retained.terminal(),
            grant_stage(grant) == Some(PublicationStage::Host)
        );
    }
    let retained = lookup(&node, &client, "node", "agent")
        .await?
        .context("retained record")?;
    assert!(retained.journal.complete());
    recovered(&node, &client).await?;
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn publication_without_a_host_grant_is_refused() -> Result<()> {
    let (node, client) = client_node().await?;
    let observer = Observer::new(
        node.clone(),
        client.clone(),
        vec![("agent".into(), "node".into())],
    );
    assert!(observer
        .before_commit(&client, &grants()[..2], &[])
        .await
        .is_err());
    assert!(lookup(&node, &client, "node", "agent").await?.is_none());
    node.shutdown().await?;
    Ok(())
}

#[test]
fn host_binding_names_the_agent_document_and_its_node() {
    let journal = ClientAgentJournal::new(
        "client".into(),
        "agent".into(),
        "node".into(),
        "agent-doc".into(),
        "node-doc".into(),
    );
    let [parent, executor, host] = grants();
    let record = Record {
        attempt: "attempt".into(),
        journal,
        parent,
        executor,
        host,
        host_credential: None,
    };
    record.validate().unwrap();
    let host = |doc: &str, relation: &str, target: &str| {
        let mut changed = record.clone();
        changed.host = ConfigRelationshipGrant::Actor {
            collection: "Agent".into(),
            doc_id: doc.into(),
            relation: relation.into(),
            target: target.into(),
        };
        changed
    };
    for changed in [
        host("agent-doc", "host", "other-node"),
        host("other-agent-doc", "host", "node"),
        host("agent-doc", "executor", "node"),
    ] {
        assert!(changed.validate().is_err(), "{:?}", changed.host);
    }
}

#[tokio::test]
async fn matching_attempt_id_cannot_replace_physical_grant_bindings() -> Result<()> {
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    crate::schema::ensure_runtime_schemas(&node).await?;
    let client = node.node_identity_did().context("client identity")?;
    let mut journal = ClientAgentJournal::new(
        client.into(),
        "agent".into(),
        "node".into(),
        "agent-doc".into(),
        "node-doc".into(),
    );
    journal.record_attempt(PublicationStage::Commit, true);
    let record = Record {
        attempt: "same-attempt".into(),
        journal,
        parent: ConfigRelationshipGrant::Resource {
            collection: "Agent".into(),
            doc_id: "agent-doc".into(),
            relation: "parent".into(),
            target_resource: "nodes".into(),
            target_doc_id: "node-doc".into(),
        },
        executor: ConfigRelationshipGrant::Actor {
            collection: "Agent".into(),
            doc_id: "agent-doc".into(),
            relation: "executor".into(),
            target: "agent".into(),
        },
        host: ConfigRelationshipGrant::Actor {
            collection: "Agent".into(),
            doc_id: "agent-doc".into(),
            relation: "host".into(),
            target: "node".into(),
        },
        host_credential: None,
    };
    record.validate()?;
    reserve(&node, std::slice::from_ref(&record)).await?;
    reserve(&node, std::slice::from_ref(&record)).await?;
    let mut different = record.clone();
    different.journal.node_doc = "different-node-doc".into();
    if let ConfigRelationshipGrant::Resource { target_doc_id, .. } = &mut different.parent {
        *target_doc_id = different.journal.node_doc.clone();
    }
    different.validate()?;
    assert!(reserve(&node, &[different]).await.is_err());
    let retained = lookup(&node, client, "node", "agent")
        .await?
        .context("retained record")?;
    assert_eq!(
        serde_json::to_value(retained)?,
        serde_json::to_value(record)?
    );
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn the_host_credential_is_retained_before_commit() -> Result<()> {
    let (node, client) = client_node().await?;
    let observer = Observer::new(
        node.clone(),
        client.clone(),
        vec![("agent".into(), "node".into())],
    );
    let grants = grants();
    let credential: HostCredential =
        serde_json::from_value(serde_json::json!({"token": "bound", "expires_at": 600}))?;
    observer
        .before_commit(&client, &grants, &[(grants[2].clone(), credential.clone())])
        .await?;
    assert_eq!(
        observer.host_credential(&grants[2]).await,
        Some(credential.clone())
    );
    assert_eq!(observer.host_credential(&grants[1]).await, None);
    let retained = lookup(&node, &client, "node", "agent")
        .await?
        .context("retained record")?;
    assert_eq!(retained.host_credential, Some(credential.clone()));
    assert!(credential.holds_at(599) && !credential.holds_at(600));
    node.shutdown().await?;
    Ok(())
}
