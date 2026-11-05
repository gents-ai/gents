use super::*;
use crate::store::config_client::{ConfigAccess, GraphqlEndpoint};
use crate::store::identity::NodeIdentity;
use serde_json::{json, Value};

fn envelope_rows<'a>(response: &'a Value, name: &str) -> anyhow::Result<&'a [Value]> {
    response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("{name} response omitted rows"))
}

fn client(endpoint: &str, identity: &crate::KeyIdentity) -> anyhow::Result<ConfigAccess> {
    Ok(ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(endpoint, identity.did())
            .with_client_signing_capability(identity.client_signing_capability()?)?,
    ))
}

/// The production publisher and SDL use the executable native-policy model.
#[tokio::test]
async fn native_mailbox_recipient_and_creator_follow_modeled_policy() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let other = crate::KeyIdentity::load_or_create(directory.path().join("other.key"), None)?;
    let binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
    let denied_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let denied_agent = crate::store::identity::resolve_agent_identity(&runtime, &denied_binding)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let modeled = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_native_policy;
    assert_eq!(
        modeled["policy_yaml"].as_str(),
        Some(gents_protocol::schemas::MAILBOX_POLICY)
    );
    let policy = node
        .add_dac_policy_with_counter_as(&context, modeled["policy_yaml"].as_str().unwrap(), 0)
        .await?;
    for actor in [agent.did(), other.did(), runtime.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), actor)
                .await?;
        }
    }
    for permission in [
        acp::nac::NodePermission::DocumentRead,
        acp::nac::NodePermission::DocumentUpdate,
        acp::nac::NodePermission::CollectionGet,
    ] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), denied_agent.did())
            .await?;
    }
    assert_eq!(policy, gents_protocol::schemas::MAILBOX_POLICY_ID);
    node.add_collection_as(&context, gents_protocol::schemas::MAILBOX_ITEM)
        .await?;
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let user_did = identity::Did::new(user.did())?;
    let input = json!({"item_key":"mailbox:modeled", "requester_did":user.did(), "node_did":runtime.did(),
        "status":"open", "kind":"flag", "action":"ack", "title":"Review",
        "source_kind":"agent", "source_id":"source", "target_node_did":runtime.did(),
        "target_agent_did":agent.did(), "created_at":"2026-11-05T00:00:00Z"});
    let mutation = "mutation($input: MailboxItemMutationInputArg!) { create_MailboxItem(input:$input) { _docID _version { cid } } }";
    let stamp_context = MailboxStampContext {
        requester_did: user.did().into(),
        node_did: runtime.did().into(),
        agent_did: agent.did().into(),
        session_id: None,
    };
    let stamp_args = FileMailboxItemArgs {
        kind: MailboxKind::Flag,
        action: MailboxAction::Ack,
        title: "Review".into(),
        summary: None,
        payload: None,
        source_kind: MailboxSourceKind::Agent,
        source_id: "source".into(),
        session_id: None,
        request_id: None,
        cause_doc_id: None,
        expected_collection: None,
        parent_item_id: None,
        deadline_at: None,
    };
    let denied_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(denied_agent.did())?,
        denied_agent.as_ref(),
    )?;
    let denied_context = MailboxStampContext {
        agent_did: denied_agent.did().into(),
        ..stamp_context.clone()
    };
    let mut denied_args = stamp_args.clone();
    denied_args.source_id = "grant-denied".into();
    assert!(stamp_notification(
        &node,
        &denied_access,
        &denied_context,
        denied_args,
        &NotificationIdentity::Event,
        MAILBOX_CLOSE_COLLECTIONS
    )
    .await
    .is_err());
    let denied_rows = denied_access.execute("{ MailboxItem { _docID } }").await?;
    assert_eq!(envelope_rows(&denied_rows, "MailboxItem")?.len(), 0);
    let receipt = stamp_notification(
        &node,
        &access,
        &stamp_context,
        stamp_args.clone(),
        &NotificationIdentity::Event,
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await?;
    assert_eq!(receipt.outcome, MailboxWriteOutcome::Created);
    let doc = receipt.item.doc_id;
    let created = access
        .execute(&format!(
            "{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _version {{ cid }} }} }}",
            escape_graphql_string(&doc)
        ))
        .await?;
    let cid = created["data"]["MailboxItem"][0]["_version"][0]["cid"]
        .as_str()
        .unwrap();
    assert_eq!(node.verified_block_signer_did(cid).await?, agent.did());
    let mut aborted_input = input.clone();
    aborted_input["item_key"] = json!("mailbox:aborted");
    let aborted: anyhow::Result<()> = access
        .transact_write("test.mailbox.abort", |txn| {
            let input = &aborted_input;
            let user_did = &user_did;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(mutation, &json!({"input":input}))
                    .await?;
                let doc = crate::store::graphql::created_doc_id(&response, "MailboxItem")?;
                txn.add_actor_relationship("MailboxItem", &doc, "recipient", user_did)
                    .await?;
                anyhow::bail!("intentional transaction abort")
            })
        })
        .await;
    assert!(aborted.is_err());
    let absent = access
        .execute("{ MailboxItem(filter: { item_key: {_eq: \"mailbox:aborted\"} }) { _docID } }")
        .await?;
    assert_eq!(
        !envelope_rows(&absent, "MailboxItem")?.is_empty(),
        modeled["aborted_document"].as_bool().unwrap()
    );
    let checks = modeled["checks"].as_array().unwrap();
    assert!(!checks.is_empty());
    let url = format!("http://{address}/api/v0/graphql");
    for case in checks {
        let actor = match case["actor"].as_str().unwrap() {
            "agent" => agent.did(),
            "user" => user.did(),
            "other" => other.did(),
            "runtime" => runtime.did(),
            role => panic!("unknown modeled mailbox role {role}"),
        };
        let permission = match case["permission"].as_str().unwrap() {
            "read" => defra_node::DocumentPermission::Read,
            "update" => defra_node::DocumentPermission::Update,
            "delete" => defra_node::DocumentPermission::Delete,
            permission => panic!("unknown modeled permission {permission}"),
        };
        let allowed = node
            .document_acp()
            .check_doc_access(
                &defra_node::AcpIdentity::Authenticated(actor.parse()?),
                permission,
                &policy,
                "mailbox_items",
                &doc,
            )
            .await?;
        assert_eq!(allowed, case["expected"].as_bool().unwrap(), "{case}");
        if case["permission"] == "read" && case["actor"] != "agent" {
            let key = match case["actor"].as_str().unwrap() {
                "user" => &user,
                "other" => &other,
                "runtime" => &runtime,
                _ => unreachable!(),
            };
            let response = client(&url, key)?
                .execute(&format!(
                    "{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                    escape_graphql_string(&doc)
                ))
                .await?;
            assert_eq!(
                !envelope_rows(&response, "MailboxItem")?.is_empty(),
                allowed,
                "HTTP visibility: {case}"
            );
        }
    }
    let user_access = client(&url, &user)?;
    let other_access = client(&url, &other)?;
    let other_update = checks
        .iter()
        .find(|case| case["actor"] == "other" && case["permission"] == "update")
        .unwrap();
    let outsider_result = dismiss_mailbox_item_on(&other_access, &doc).await;
    assert_eq!(
        outsider_result.is_ok(),
        other_update["expected"].as_bool().unwrap()
    );
    assert_eq!(
        load_mailbox_item_with_access(&user_access, &doc)
            .await?
            .unwrap()
            .status,
        "open"
    );
    access
        .transact_write("test.mailbox.revoke_before_duplicate", |txn| {
            let doc = &doc;
            let user_did = &user_did;
            Box::pin(async move {
                txn.delete_actor_relationship("MailboxItem", doc, "recipient", user_did)
                    .await
            })
        })
        .await?;
    let duplicate = stamp_notification(
        &node,
        &access,
        &stamp_context,
        stamp_args,
        &NotificationIdentity::Event,
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await?;
    assert_eq!(duplicate.outcome, MailboxWriteOutcome::Reused);
    assert_eq!(duplicate.item.doc_id, doc);
    assert!(load_mailbox_item_with_access(&user_access, &doc)
        .await?
        .is_none());
    access
        .transact_write("test.mailbox.explicit_restore", |txn| {
            let doc = &doc;
            let user_did = &user_did;
            Box::pin(async move {
                txn.add_actor_relationship("MailboxItem", doc, "recipient", user_did)
                    .await
            })
        })
        .await?;
    let dismissed = dismiss_mailbox_item_on(&user_access, &doc).await?;
    assert_eq!(dismissed.status, "dismissed");
    let response = user_access
        .execute(&format!(
            "{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _version {{ cid }} }} }}",
            escape_graphql_string(&doc)
        ))
        .await?;
    let cid = response["data"]["MailboxItem"][0]["_version"][0]["cid"]
        .as_str()
        .unwrap();
    assert_eq!(node.verified_block_signer_did(cid).await?, user.did());
    access
        .transact_write("test.mailbox.revoke", |txn| {
            let doc = &doc;
            let user_did = &user_did;
            Box::pin(async move {
                txn.delete_actor_relationship("MailboxItem", doc, "recipient", user_did)
                    .await
            })
        })
        .await?;
    let revoked = user_access
        .execute(&format!(
            "{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            escape_graphql_string(&doc)
        ))
        .await?;
    let modeled_revocation = checks
        .iter()
        .find(|case| case["actor"] == "user" && case["permission"] == "read")
        .unwrap();
    assert_eq!(
        !envelope_rows(&revoked, "MailboxItem")?.is_empty(),
        modeled_revocation["revoked_expected"].as_bool().unwrap()
    );
    assert!(dismiss_mailbox_item_on(&user_access, &doc).await.is_err());
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn mailbox_sweep_uses_held_targets() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let first_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let second_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let first = crate::store::identity::resolve_agent_identity(&runtime, &first_binding)?;
    let second = crate::store::identity::resolve_agent_identity(&runtime, &second_binding)?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .build()
            .await?,
    );
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, gents_protocol::schemas::MAILBOX_POLICY, 0)
        .await?;
    assert_eq!(policy, gents_protocol::schemas::MAILBOX_POLICY_ID);
    node.add_collection_as(&context, gents_protocol::schemas::MAILBOX_ITEM)
        .await?;
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for actor in [first.did(), second.did()] {
        for permission in [
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::DacRelationAdd,
            crate::store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse()?, permission)
                .await?;
        }
    }
    let mut ids = Vec::new();
    for (identity, label) in [(&first, "first"), (&second, "second")] {
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity.did().parse()?,
            identity.as_ref(),
        )?;
        let scope = MailboxStampContext {
            requester_did: user.did().into(),
            node_did: runtime.did().into(),
            agent_did: identity.did().into(),
            session_id: None,
        };
        for index in 0..20 {
            let source = format!("{index:02}-{label}");
            let args = FileMailboxItemArgs {
                kind: MailboxKind::Flag,
                action: MailboxAction::Ack,
                title: source.clone(),
                summary: None,
                payload: None,
                source_kind: MailboxSourceKind::Agent,
                source_id: source,
                session_id: None,
                request_id: None,
                cause_doc_id: None,
                expected_collection: None,
                parent_item_id: None,
                deadline_at: Some("2000-01-01T00:00:00Z".into()),
            };
            ids.push(
                stamp_notification(
                    &node,
                    &access,
                    &scope,
                    args,
                    &NotificationIdentity::Event,
                    MAILBOX_CLOSE_COLLECTIONS,
                )
                .await?
                .item
                .doc_id,
            );
        }
    }
    let query = format!(
        "{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
        escape_graphql_string(&ids[0])
    );
    for case in &crate::lean_vocab_test::lean_contract_snapshot().mailbox_sweep_actor_cases {
        let access = match case["actor"].as_str().unwrap() {
            "agent" => ConfigAccess::local_managed_agent(
                node.clone(),
                first.did().parse()?,
                first.as_ref(),
            )?,
            "other" => ConfigAccess::local_managed_agent(
                node.clone(),
                second.did().parse()?,
                second.as_ref(),
            )?,
            "node" => admin.clone(),
            actor => anyhow::bail!("unknown modeled actor {actor}"),
        };
        let rows = access.execute(&query).await?;
        assert_eq!(
            !rows["data"]["MailboxItem"].as_array().unwrap().is_empty(),
            case["visible"].as_bool().unwrap(),
            "{case}"
        );
    }
    let mut cursor = None;
    let mut scanned = 0;
    let mut expired = 0;
    for _ in 0..100 {
        let page = sweep_open_mailbox_items(
            &node,
            runtime.did(),
            &[first.clone(), second.clone()],
            cursor.as_deref(),
        )
        .await?;
        scanned += page.report.scanned;
        expired += page.report.expired;
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "mailbox pagination did not finish");
    assert_eq!(scanned, ids.len());
    assert_eq!(expired, ids.len());
    node.shutdown().await?;
    Ok(())
}
