use std::sync::Arc;

use super::*;

const OWNER: &str = "did:key:zPackInstallOwner";

async fn access() -> ConfigAccess {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    ConfigAccess::Local(node)
}

fn config(tools: &[(&str, &str)]) -> PackConfig {
    serde_json::from_value(json!({
        "agent_principal": {"agent_did": OWNER},
        "tools": tools
            .iter()
            .map(|(id, name)| json!({"tools_id": id, "display_name": name, "agent_did": OWNER, "tags": ["gents:pack:demo"]}))
            .collect::<Vec<_>>(),
    }))
    .unwrap()
}

fn identity(version: &str) -> PackIdentity {
    PackIdentity {
        coordinate: "acme/demo".into(),
        version: version.into(),
        digest: format!(
            "sha256:{}",
            version.repeat(64).chars().take(64).collect::<String>()
        ),
    }
}

async fn install(
    access: &ConfigAccess,
    version: &str,
    config: &PackConfig,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    super::super::install_pack_documents(access, OWNER, &identity(version), config, policy).await
}

async fn edit_tools(access: &ConfigAccess, id: &str, name: &str) {
    let edit = crate::config_client::DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: json!({"tools_id": id, "display_name": name, "agent_did": OWNER}),
        update: json!({"tools_id": id, "display_name": name, "agent_did": OWNER}),
    }])
    .unwrap();
    access
        .transact("test.edit", |txn| {
            let edit = &edit;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, edit).await })
        })
        .await
        .unwrap();
}

async fn tools_ids(access: &ConfigAccess) -> Vec<String> {
    let response = access.execute("{ Tools { tools_id } }").await.unwrap();
    let mut ids: Vec<String> = response["data"]["Tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["tools_id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn install_then_remove_leaves_no_pack_documents_and_keeps_adopted_ones() {
    let access = access().await;
    edit_tools(&access, "shared", "Shared before the pack").await;

    let report = install(
        &access,
        "1",
        &config(&[("alpha", "Alpha"), ("shared", "Shared")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert_eq!(report.created, vec!["Tools/alpha"]);
    assert_eq!(report.adopted, vec!["Tools/shared"]);

    let removed = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(removed.removed, vec!["Tools/alpha"]);
    assert_eq!(
        tools_ids(&access).await,
        vec!["shared"],
        "an adopted document stays"
    );
    let record = access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap();
    assert_eq!(record["data"]["PackInstallation"], json!([]));
    assert!(
        remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_edited_document_stops_an_upgrade_by_name() {
    let access = access().await;
    let v1 = config(&[("alpha", "Alpha"), ("beta", "Beta")]);
    install(&access, "1", &v1, DriftPolicy::Refuse)
        .await
        .unwrap();
    // Reinstalling what is installed replaces nothing it should not.
    let again = install(&access, "1", &v1, DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(again.replaced, vec!["Tools/alpha", "Tools/beta"]);

    edit_tools(&access, "alpha", "Alpha, edited by the operator").await;
    let v2 = config(&[("alpha", "Alpha 2"), ("beta", "Beta 2")]);
    let error = install(&access, "2", &v2, DriftPolicy::Refuse)
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("Tools/alpha"), "{message}");
    assert!(!message.contains("Tools/beta"), "{message}");

    let kept = install(&access, "2", &v2, DriftPolicy::Keep).await.unwrap();
    assert_eq!(kept.kept, vec!["Tools/alpha"]);
    assert_eq!(kept.replaced, vec!["Tools/beta"]);
    let alpha = access
        .execute(r#"{ Tools(filter: {tools_id: {_eq: "alpha"}}) { display_name } }"#)
        .await
        .unwrap();
    assert_eq!(
        alpha["data"]["Tools"][0]["display_name"],
        "Alpha, edited by the operator"
    );

    let overwritten = install(&access, "2", &v2, DriftPolicy::Overwrite)
        .await
        .unwrap();
    assert_eq!(overwritten.replaced, vec!["Tools/alpha", "Tools/beta"]);
}

#[tokio::test]
async fn a_document_the_new_version_drops_is_removed_and_history_is_kept() {
    let access = access().await;
    install(
        &access,
        "1",
        &config(&[("alpha", "Alpha"), ("beta", "Beta")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let report = install(
        &access,
        "2",
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert_eq!(report.removed, vec!["Tools/beta"]);
    assert_eq!(tools_ids(&access).await, vec!["alpha"]);
    let record = access
        .execute("{ PackInstallation { version digest history } }")
        .await
        .unwrap();
    let row = &record["data"]["PackInstallation"][0];
    assert_eq!(row["version"], "2");
    assert_eq!(row["history"], json!([identity("1").digest]));
}
