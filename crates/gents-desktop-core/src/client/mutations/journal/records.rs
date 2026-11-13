use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::views::journals::{PendingSubmission, PendingSubmissionRow};

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

fn row(rows: Vec<PendingSubmissionRow>, request_id: &str) -> Result<Option<(String, Entry)>> {
    anyhow::ensure!(rows.len() <= 1, "retained submission identity is ambiguous");
    rows.into_iter()
        .next()
        .map(|row| Ok((row.doc_id, decode(&row.payload, request_id)?)))
        .transpose()
}

async fn read_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> Result<Option<(String, Entry)>> {
    row(
        txn.read_view(&PendingSubmission { request_id }).await?,
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
        let cases = tokio::task::spawn_blocking(|| {
            gents_lean_vocab::families::client_agent_journal_local_signer::cases()
        })
        .await
        .unwrap();
        for case in cases {
            let actor = match case.input.role.as_str() {
                "client" => client.did(),
                "other" => other.did(),
                "empty" => "",
                role => panic!("unknown modeled signer {role}"),
            };
            case.check(require_client_signer(&node, actor).is_ok());
        }
        node.shutdown().await.unwrap();
    }
}
