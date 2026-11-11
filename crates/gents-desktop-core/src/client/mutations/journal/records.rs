use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

pub(crate) mod goal;
pub(crate) mod publication;

pub(crate) fn require_client_signer(node: &EmbeddedNode, client: &str) -> Result<()> {
    anyhow::ensure!(
        !client.is_empty() && node.node_identity_did() == Some(client),
        "client journal actor differs from the local client signer"
    );
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "record", deny_unknown_fields)]
enum Envelope {
    #[serde(rename = "goalCallerReceipt-v1")]
    Goal(Box<super::goal_delivery::Record>),
    #[serde(rename = "agentPublication-v1")]
    Publication(Box<publication::Record>),
}

enum Entry {
    Goal(Box<super::goal_delivery::Record>),
    Publication(Box<publication::Record>),
}

fn decode(payload: &str, request_id: &str) -> Result<Entry> {
    let envelope: Envelope =
        serde_json::from_str(payload).context("decoding a client journal record")?;
    match envelope {
        Envelope::Goal(record) => {
            anyhow::ensure!(
                record.intent.command_id == request_id
                    && record.decision() != super::goal_delivery::Decision::Refused,
                "retained Goal caller binding is invalid"
            );
            Ok(Entry::Goal(record))
        }
        Envelope::Publication(record) => {
            anyhow::ensure!(
                record.key() == request_id,
                "retained publication binding is invalid"
            );
            record.validate()?;
            Ok(Entry::Publication(record))
        }
    }
}

fn query(request_id: &str) -> String {
    let id = escape_graphql_string(request_id);
    format!("{{ PendingChatSubmission(filter: {{request_id: {{_eq: \"{id}\"}}}}, limit: 2) {{_docID payload}} }}")
}

fn row(data: &serde_json::Value, request_id: &str) -> Result<Option<(String, Entry)>> {
    let rows = data
        .get("PendingChatSubmission")
        .and_then(serde_json::Value::as_array)
        .context("pending submission query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "retained submission identity is ambiguous");
    rows.first()
        .map(|row| {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .context("pending submission has no document identity")?;
            let payload = row
                .get("payload")
                .and_then(serde_json::Value::as_str)
                .context("pending submission has no payload")?;
            Ok((doc_id.to_owned(), decode(payload, request_id)?))
        })
        .transpose()
}

async fn read_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> Result<Option<(String, Entry)>> {
    let response = txn.execute(&query(request_id)).await?;
    row(
        response
            .get("data")
            .context("pending submission query omitted data")?,
        request_id,
    )
}

fn require_written(response: &serde_json::Value, field: &str) -> Result<()> {
    let rows = gents_store::graphql::mutation_documents(response, field)?
        .context("client journal mutation omitted its result")?;
    anyhow::ensure!(
        rows.len() == 1,
        "client journal mutation did not write exactly one document"
    );
    Ok(())
}

#[cfg(test)]
mod mutation_result_tests {
    use super::require_written;

    #[test]
    fn journal_mutation_requires_one_written_document() {
        let empty = serde_json::json!({"data": {"update_PendingChatSubmission": []}});
        assert!(require_written(&empty, "update_PendingChatSubmission").is_err());
        let written = serde_json::json!({"data": {"update_PendingChatSubmission": [{"_docID": "owner-document"}]}});
        require_written(&written, "update_PendingChatSubmission").unwrap();
    }
}

#[cfg(test)]
mod signer_tests {
    use super::require_client_signer;
    use defra_node::Node as EmbeddedNode;
    use gents_store::identity::NodeIdentity as _;

    /// `Identity.ClientAgentJournal.privatePolicy`: only the local client key
    /// may write its private journal.
    #[tokio::test]
    async fn client_journal_refuses_a_different_local_signer() {
        let home = tempfile::tempdir().unwrap();
        let client = gents_store::identity::KeyIdentity::load_or_create(
            home.path().join("client.key"),
            None,
        )
        .unwrap();
        let other =
            gents_store::identity::KeyIdentity::load_or_create(home.path().join("other.key"), None)
                .unwrap();
        let node = EmbeddedNode::builder()
            .with_node_identity_did(client.did())
            .build()
            .await
            .unwrap();
        let snapshot = tokio::task::spawn_blocking(
            gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
        )
        .await
        .unwrap()
        .unwrap();
        let checks = snapshot["identity_private_journal_policy"]["signer_checks"]
            .as_array()
            .unwrap();
        assert!(!checks.is_empty());
        for case in checks {
            let actor = match case["role"].as_str().unwrap() {
                "client" => client.did(),
                "other" => other.did(),
                "empty" => "",
                _ => panic!("unknown modeled signer"),
            };
            assert_eq!(
                require_client_signer(&node, actor).is_ok(),
                case["expected"].as_bool().unwrap()
            );
        }
        node.shutdown().await.unwrap();
    }
}
