use super::*;

#[test]
fn plugin_writes_failure_keeps_a_typed_conflict_replayable() {
    let conflict = crate::store::config_client::classified_conflict_for_test();
    let wrapped = plugin_writes_failed(conflict.into());
    assert!(crate::store::config_client::is_classified_transaction_conflict(&wrapped));
    assert!(wrapped.downcast_ref::<PluginWritesFailed>().is_none());

    let refused = plugin_writes_failed(anyhow::anyhow!("collection is not declared"));
    assert!(!crate::store::config_client::is_classified_transaction_conflict(&refused));
    assert!(refused.downcast_ref::<PluginWritesFailed>().is_some());
}

#[tokio::test]
async fn denied_plugin_write_rolls_back_completed_terminal_and_settles_failed() {
    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use crate::store::config_client::ConfigAccess;
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use gents_protocol::job::{DataOp, DataWrite, PluginDataDeclaration};
    use gents_protocol::output::OutputSource;
    use serde_json::json;
    use std::collections::BTreeMap;

    let (mut fixture, _request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "plugin-acp-terminal-denial".into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    // The synthetic target belongs to the node so any authority fallback would succeed.
    let owner = fixture.node_did.clone();
    assert_ne!(fixture.tool.actor.to_string(), owner);
    let policy = fixture.node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(owner.clone())),
            "name: Terminal rollback ACP\nresources:\n  - name: parcels\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
        )
        .await
        .unwrap();
    fixture
        .node
        .add_collection(&format!(
            "type TerminalParcel @policy(id: \"{}\", resource: \"parcels\") {{ status: String }}\n             type TerminalNote {{ body: String }}",
            crate::store::graphql::escape_graphql_string(&policy)
        ))
        .await
        .unwrap();
    let owner_access = ConfigAccess::local_as(
        fixture.node.clone(),
        identity::Did::new(owner.as_str()).unwrap(),
    );
    let created = owner_access
        .write(
            "test.plugin_acp_parcel",
            "mutation { create_TerminalParcel(input: { status: \"original\" }) { _docID } }",
        )
        .await
        .unwrap();
    let parcel = crate::store::graphql::created_doc_id(&created, "TerminalParcel").unwrap();
    fixture
        .node
        .add_dac_actor_relationship(
            owner.as_str(),
            "TerminalParcel",
            &parcel,
            "reader",
            fixture.tool.actor.as_ref(),
        )
        .await
        .unwrap();
    let actor_access = ConfigAccess::local_as(fixture.node.clone(), fixture.tool.actor.clone());
    let visible_parcel = actor_access
        .execute("{ TerminalParcel { status } }")
        .await
        .unwrap();
    assert_eq!(
        visible_parcel["data"]["TerminalParcel"][0]["status"],
        "original"
    );
    fixture.tool.plugin_writes = Some(PluginToolWrites {
        actor: fixture.tool.actor.clone(),
        grant: crate::plugin::data::PluginData {
            declaration: PluginDataDeclaration {
                collections: vec!["TerminalNote".into(), "TerminalParcel".into()],
                ops: vec!["insert".into(), "update".into()],
                stamped: BTreeMap::new(),
            },
            collection_fields: BTreeMap::from([
                ("TerminalNote".into(), vec!["body".into()]),
                ("TerminalParcel".into(), vec!["status".into()]),
            ]),
            unique_keys: BTreeMap::new(),
        },
        writes: vec![
            DataWrite {
                op: DataOp::Insert,
                collection: "TerminalNote".into(),
                doc_id: None,
                fields: json!({"body": "should roll back"})
                    .as_object()
                    .unwrap()
                    .clone(),
                on_conflict: None,
            },
            DataWrite {
                op: DataOp::Update,
                collection: "TerminalParcel".into(),
                doc_id: Some(parcel),
                fields: json!({"status": "should roll back"})
                    .as_object()
                    .unwrap()
                    .clone(),
                on_conflict: None,
            },
        ],
    });
    let pending = fixture.tool.plugin_writes.as_ref().unwrap();
    let grant = pending.grant.clone();
    let actor = pending.actor.clone();
    let stamps = crate::plugin::data::StampContext::ToolOutput {
        caller_did: actor.to_string(),
        session_id: fixture.tool.session_id.clone(),
        request_doc_id: fixture.tool.request_doc_id.clone().unwrap(),
        tool_call_doc_id: fixture.tool.doc_id().unwrap().to_owned(),
    };
    crate::plugin::data::admit_write(&grant, &stamps, pending.writes.clone())
        .expect("both writes pass the plugin grant before ACP is checked");
    let mut baseline = pending.writes[0].clone();
    baseline.fields.insert("body".into(), json!("baseline"));
    ConfigAccess::transact_local_write(
        &fixture.node,
        Some(actor.clone()),
        "test.plugin_permitted_baseline",
        |txn| {
            let actor = actor.clone();
            let baseline = baseline.clone();
            let grant = grant.clone();
            Box::pin(async move {
                let stamps = crate::plugin::data::StampContext::Callback {
                    caller_did: actor.to_string(),
                };
                crate::plugin::application_write::apply_plugin_writes_in_txn(
                    txn,
                    &actor,
                    &grant,
                    &stamps,
                    vec![baseline],
                )
                .await
            })
        },
    )
    .await
    .expect("the first plugin write is permitted for the same actor");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fixture.tool.complete("successful plugin output"),
    )
    .await
    .expect("ACP denial must settle without retrying the refused plugin write")
    .unwrap();
    assert_eq!(
        fixture.tool.state,
        crate::tools::call::lifecycle::ToolCallState::Failed
    );
    assert_eq!(
        fixture.tool.failure_class,
        Some(crate::tools::call::lifecycle::FailureClass::ToolReturnedError)
    );
    let parcel = owner_access
        .execute("{ TerminalParcel { status } }")
        .await
        .unwrap();
    assert_eq!(parcel["data"]["TerminalParcel"][0]["status"], "original");
    let notes = actor_access
        .execute("{ TerminalNote { body } }")
        .await
        .unwrap();
    assert_eq!(
        notes["data"]["TerminalNote"],
        json!([{ "body": "baseline" }])
    );
    let doc_id = crate::store::graphql::escape_graphql_string(fixture.tool.doc_id().unwrap());
    let terminal = ConfigAccess::local_as(fixture.node.clone(), fixture.tool.actor.clone()).execute(&format!(
        "{{ AgentToolCall(filter: {{ _docID: {{ _eq: \"{doc_id}\" }} }}) {{ lifecycle_state tool_failure_class }} }}"
    )).await.unwrap();
    assert_eq!(
        terminal["data"]["AgentToolCall"][0]["lifecycle_state"],
        "failed"
    );
    assert_eq!(
        terminal["data"]["AgentToolCall"][0]["tool_failure_class"],
        crate::tools::call::lifecycle::FailureClass::ToolReturnedError.as_str()
    );
    let request_doc_id = crate::store::graphql::escape_graphql_string(
        fixture.tool.request_doc_id.as_deref().unwrap(),
    );
    let segments = actor_access.execute(&format!(
        "{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: \"{request_doc_id}\" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"
    )).await.unwrap();
    let closures = segments["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .filter(|row| {
            row.segment.source
                == OutputSource::ToolCall {
                    tool_call_doc_id: fixture.tool.doc_id().unwrap().to_owned(),
                }
                && row.segment.close.is_some()
        })
        .count();
    assert_eq!(closures, 1);
    fixture.node.shutdown().await.expect("shut down test node");
}
