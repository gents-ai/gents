use gents_p2p::p2p_reconcile::{
    decide_operator_command, ensure_enrollment_network, sign_operator_command,
    GraphqlEnrollmentStore,
};
use gents_protocol::enrollment::EnrollmentOperatorAction;
use gents_store::config_client::ConfigAccess;
use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::{DocumentAcp, DocumentAcpError};

use super::{home, start};

async fn network_ids(access: &ConfigAccess) -> anyhow::Result<Vec<String>> {
    let response = access.execute("{ Network { network_id } }").await?;
    Ok(response["data"]["Network"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["network_id"].as_str().map(str::to_owned))
        .collect())
}

/// Lean `Identity.NodePublication.a_vera_home_publishes_no_network_row` and
/// `a_vera_home_refuses_every_enrollment_request`: the serve-start step that
/// creates the enrollment root writes no `Network` row on a Vera home, which
/// goes on answering its runtime, and an operator decision is refused saying
/// peer networking is not available there.
/// `a_home_on_the_node_store_enrolls_as_before`: a local home creates its root
/// once and reloads it on the next start.
#[tokio::test]
async fn serving_creates_an_enrollment_root_only_on_a_local_home() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, provider) in [("vera", DocumentAcp::Vera), ("local", DocumentAcp::Local)] {
        let home = home(directory.path(), name).await?;
        FreshBackendStore::observe(&home.data, &home.lock)
            .await?
            .initialize(&home.key, &home.node, &home.owner, provider)
            .await?;
        let node = start(&home, provider).await?;
        let node_did = home.node.did().to_owned();
        let access = ConfigAccess::local_as(node.clone(), identity::Did::new(&node_did)?);
        let root = ensure_enrollment_network(&node, home.node.as_ref(), "home").await?;
        let again = ensure_enrollment_network(&node, home.node.as_ref(), "home").await?;
        let command = sign_operator_command(
            home.node.as_ref(),
            "absent",
            EnrollmentOperatorAction::Deny,
            0,
        )
        .await?;
        let decided = decide_operator_command(
            home.node.as_ref(),
            &node,
            &GraphqlEnrollmentStore::new(node.clone(), home.node.clone()),
            &command,
        )
        .await
        .expect_err("no enrollment request is pending");
        let refusal = DocumentAcpError::in_chain(decided.as_ref());
        match provider {
            DocumentAcp::Vera => {
                assert!(root.is_none() && again.is_none(), "{root:?} {again:?}");
                assert!(network_ids(&access).await?.is_empty());
                assert!(
                    matches!(
                        refusal,
                        Some(DocumentAcpError::TransportGrantsAwaitProvider)
                    ),
                    "{decided:#}"
                );
                assert!(
                    decided
                        .to_string()
                        .starts_with("peer networking is not available on this home yet"),
                    "{decided}"
                );
            }
            DocumentAcp::Local => {
                let root = root.expect("a local home creates its enrollment root");
                assert_eq!(
                    again.map(|again| again.network_id),
                    Some(root.network_id.clone())
                );
                assert_eq!(network_ids(&access).await?, [root.network_id]);
                assert!(refusal.is_none(), "{decided:#}");
            }
        }
        access
            .write_with_variables(
                "test.enrollment_root.node",
                "mutation($node: String) { create_Node(input: {node_did: $node}) { _docID } }",
                &serde_json::json!({ "node": node_did }),
            )
            .await?;
        let nodes = access.execute("{ Node { node_did } }").await?;
        assert_eq!(nodes["data"]["Node"][0]["node_did"], node_did.as_str());
        node.shutdown().await?;
    }
    Ok(())
}
