use std::sync::Arc;

use acp::{
    policy_yaml::{build_policy, parse_policy_yaml, validate_policy_expressions},
    validate_resource_interface, DocumentACP, DocumentPermission, Identity, MemoryZanzibarStore,
    StorePolicyOptions, ZanzibarDocumentACP, ZanzibarStore,
};
use identity::Did;

const PROJECTION_RUNTIME_RESOURCES: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentResponse",
    "AgentConversation",
    "AgentSession",
];

fn projection_policy_yaml() -> String {
    let mut yaml = String::from(
        r#"name: projection_adapter_read_policy
description: Policy fixture for Gents projection adapter exports.
resources:
"#,
    );
    for resource in PROJECTION_RUNTIME_RESOURCES {
        yaml.push_str(&format!(
            r#"- name: {resource}
  permissions:
  - name: read
    expr: reader
  - name: update
    expr: updater
  - name: delete
    expr: deleter
  relations:
  - name: reader
    types: [actor]
  - name: updater
    types: [actor]
  - name: deleter
    types: [actor]
  - name: admin
    manages: [reader, updater, deleter]
    types: [actor]
"#
        ));
    }
    yaml
}

fn did(value: &str) -> Did {
    Did::new(value).expect("test DID should be valid")
}

#[tokio::test]
async fn projection_acp_policy_resources_drive_document_decisions() -> anyhow::Result<()> {
    let yaml = projection_policy_yaml();
    let parsed = parse_policy_yaml(&yaml).map_err(anyhow::Error::msg)?;
    validate_policy_expressions(&parsed).map_err(anyhow::Error::msg)?;
    let policy = build_policy(&parsed, 1)?;
    let policy_id = policy.id.clone();

    let store = Arc::new(MemoryZanzibarStore::new());
    let options = StorePolicyOptions::new()
        .with_validation()
        .with_dpi_enforcement();
    store.store_policy_with_options(&policy, &options).await?;

    for resource in PROJECTION_RUNTIME_RESOURCES {
        validate_resource_interface(&policy_id, resource, Some(&policy))
            .map_err(anyhow::Error::msg)?;
    }

    let acp = ZanzibarDocumentACP::new(store);
    let owner = did("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK");
    let reader = did("did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH");
    let stranger = did("did:key:z6MknSLrJoTcukLrE435hVNQT4JUhbvWLX4kUzqkEStBU8Vi");
    let resource_name = "AgentRequest";
    let doc_id = "req-projection-acp-lifecycle-1";

    acp.register_doc_object(&owner, &policy_id, resource_name, doc_id)
        .await?;

    assert!(
        acp.check_doc_access(
            &Identity::Authenticated(owner.clone()),
            DocumentPermission::Read,
            &policy_id,
            resource_name,
            doc_id,
        )
        .await?,
        "document owner should read registered projection rows"
    );
    assert!(
        !acp.check_doc_access(
            &Identity::Authenticated(reader.clone()),
            DocumentPermission::Read,
            &policy_id,
            resource_name,
            doc_id,
        )
        .await?,
        "reader should be denied before a relationship is granted"
    );

    let added = acp
        .add_actor_relationship(
            &owner,
            &reader,
            &policy_id,
            resource_name,
            doc_id,
            "reader",
            &[],
        )
        .await?;
    assert!(added, "reader relationship should be created");

    assert!(
        acp.check_doc_access(
            &Identity::Authenticated(reader.clone()),
            DocumentPermission::Read,
            &policy_id,
            resource_name,
            doc_id,
        )
        .await?,
        "reader relationship should allow projection row reads"
    );
    assert!(
        !acp.check_doc_access(
            &Identity::Authenticated(reader.clone()),
            DocumentPermission::Update,
            &policy_id,
            resource_name,
            doc_id,
        )
        .await?,
        "reader relationship must not imply update"
    );
    assert!(
        !acp.check_doc_access(
            &Identity::Authenticated(stranger),
            DocumentPermission::Read,
            &policy_id,
            resource_name,
            doc_id,
        )
        .await?,
        "unrelated actors should remain denied"
    );

    // (defra.rs #1033 removed `export_actor_relationships`; the grant is fully
    // verified above via check_doc_access — reader reads but cannot update, and
    // strangers are denied — and below by re-checking after deletion.)
    let deleted = acp
        .delete_actor_relationship(
            &owner,
            &reader,
            &policy_id,
            resource_name,
            doc_id,
            "reader",
            &[],
        )
        .await?;
    assert!(deleted, "reader relationship should be deleted");
    assert!(
        !acp.check_doc_access(
            &Identity::Authenticated(reader),
            DocumentPermission::Read,
            &policy_id,
            resource_name,
            doc_id,
        )
        .await?,
        "deleted relationship should revoke projection row reads"
    );

    Ok(())
}
