//! Explicit Tools pin edits carried by the install's own transaction, never a
//! pre-install write. Both signers reconstruct the same before/after documents;
//! the native transaction re-reads the full before value before applying it.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use gents_documents::{read_desired_state_record_in_txn, write_tools_document_in_txn, Tools};
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde::Serialize;

use super::archive::PackArchive;

#[derive(Clone, Debug, Serialize)]
pub struct PluginPinMove {
    before: Tools,
    after: Tools,
}

fn move_pin(named: bool, replacement: bool, pinned: bool) -> bool {
    named && replacement && pinned
}

impl PluginPinMove {
    pub fn tools_id(&self) -> &str {
        &self.before.tools_id
    }
}

/// Archive bytes, rather than whatever plugin happens to be installed, own the
/// replacement digests. A repeated coordinate must have exactly one digest.
pub fn replacement_digests<'a>(
    archives: impl IntoIterator<Item = &'a PackArchive>,
) -> Result<BTreeMap<String, String>> {
    let mut replacements = BTreeMap::new();
    for archive in archives {
        let manifest = archive.manifest();
        for plugin in &manifest.metadata.plugins {
            let coordinate = format!("{}/{}", manifest.metadata.namespace, plugin.name);
            let digest = format!(
                "sha256:{}",
                crate::plugin::store::content_digest_hex(archive.asset(&plugin.artifact)?)
            );
            if let Some(previous) = replacements.insert(coordinate.clone(), digest.clone()) {
                anyhow::ensure!(
                    previous == digest,
                    "reviewed archives disagree on plugin {coordinate}"
                );
            }
        }
    }
    Ok(replacements)
}

fn plan(before: Tools, replacements: &BTreeMap<String, String>) -> PluginPinMove {
    let mut after = before.clone();
    for reference in after
        .integrations
        .as_mut()
        .and_then(|i| i.plugins.as_mut())
        .into_iter()
        .flatten()
    {
        let replacement = replacements.get(&reference.plugin);
        if move_pin(true, replacement.is_some(), reference.digest.is_some()) {
            reference.digest = replacement.cloned();
        }
    }
    PluginPinMove { before, after }
}

pub async fn prepare(
    access: &ConfigAccess,
    owner: &str,
    tools: &[String],
    replacements: &BTreeMap<String, String>,
) -> Result<Vec<PluginPinMove>> {
    anyhow::ensure!(
        tools.iter().collect::<BTreeSet<_>>().len() == tools.len(),
        "--move-pin-tools must name each Tools document only once"
    );
    if tools.is_empty() {
        return Ok(Vec::new());
    }
    let mut tools = tools.to_vec();
    tools.sort();
    access
        .transact_readonly("pack.pin_moves.prepare", |txn| {
            let tools = &tools;
            Box::pin(async move {
                let mut edits = Vec::new();
                for id in tools {
                    let (_, value) =
                        read_desired_state_record_in_txn(txn, Collection::Tools, owner, id)
                            .await?
                            .with_context(|| {
                                format!("named Tools {id:?} is missing or not readable")
                            })?;
                    let before: Tools = serde_json::from_value(value)?;
                    let edit = plan(before, replacements);
                    anyhow::ensure!(
                        edit.before != edit.after,
                        "named Tools {id:?} has no pin changed by this archive graph"
                    );
                    edits.push(edit);
                }
                Ok(edits)
            })
        })
        .await
}

pub async fn apply(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    edits: &[PluginPinMove],
    policy: &dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<()> {
    for edit in edits {
        anyhow::ensure!(
            edit.before.node_did == owner && edit.after.node_did == owner,
            "pin edit belongs to another node"
        );
        let (_, value) =
            read_desired_state_record_in_txn(txn, Collection::Tools, owner, edit.tools_id())
                .await?
                .context("named Tools disappeared after pin preview")?;
        let current: Tools = serde_json::from_value(value)?;
        anyhow::ensure!(
            current == edit.before,
            "Tools {:?} changed after pin preview; nothing installed, preview again",
            edit.tools_id()
        );
        write_tools_document_in_txn(txn, &edit.after, policy).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_move_replays_the_lean_owner() {
        gents_lean_vocab::families::tool_policy_plugin_publication_move_pin::replay(|i| {
            move_pin(i.named, i.replacement, i.pinned)
        });
    }

    #[test]
    fn named_pin_moves_preserve_all_other_authority_and_unpinned_entries() {
        let tools: Tools = serde_json::from_value(serde_json::json!({
            "tools_id":"named", "node_did":"did:key:owner",
            "host":{"files":{"mode":"ReadOnly"}},
            "integrations":{"plugins":[
                {"plugin":"acme/one","digest":"old"},
                {"plugin":"acme/two","digest":"other"},
                {"plugin":"acme/three"}
            ]}
        }))
        .unwrap();
        let edit = plan(
            tools.clone(),
            &BTreeMap::from([
                ("acme/one".into(), "new".into()),
                ("acme/three".into(), "new-three".into()),
            ]),
        );
        let mut expected = tools;
        expected
            .integrations
            .as_mut()
            .unwrap()
            .plugins
            .as_mut()
            .unwrap()[0]
            .digest = Some("new".into());
        assert_eq!(edit.after, expected);
    }
    async fn pin_fixture() -> (
        tempfile::TempDir,
        gents_store::identity::KeyIdentity,
        std::sync::Arc<defra_node::Node>,
        ConfigAccess,
    ) {
        let fixture = crate::pack::agent_bindings::tests::fixture().await;
        use gents_store::identity::NodeIdentity;
        for id in ["named", "not-named"] {
            let tools: Tools = serde_json::from_value(serde_json::json!({
                "tools_id":id, "node_did":fixture.1.did(),
                "integrations":{"plugins":[{"plugin":"acme/one","digest":format!("sha256:{}", "1".repeat(64))}]}
            })).unwrap();
            gents_documents::write_tools_document(
                &fixture.3,
                &tools,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
            .unwrap();
        }
        fixture
    }

    async fn read_tools(access: &ConfigAccess, owner: &str, id: &str) -> Tools {
        access
            .transact_readonly("test.pin_moves.read", |txn| {
                Box::pin(async move {
                    let (_, value) =
                        read_desired_state_record_in_txn(txn, Collection::Tools, owner, id)
                            .await?
                            .unwrap();
                    Ok::<_, anyhow::Error>(serde_json::from_value::<Tools>(value)?)
                })
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn tools_not_named_keep_their_old_pin() {
        use gents_store::identity::NodeIdentity;
        let (_directory, runtime, node, access) = pin_fixture().await;
        let owner = runtime.did();
        let omitted = read_tools(&access, owner, "not-named").await;
        let replacement = format!("sha256:{}", "2".repeat(64));
        let edits = prepare(
            &access,
            owner,
            &["named".into()],
            &BTreeMap::from([("acme/one".into(), replacement.clone())]),
        )
        .await
        .unwrap();
        access
            .transact_write("test.pin_moves.apply", |txn| {
                let edits = &edits;
                Box::pin(async move {
                    apply(
                        txn,
                        owner,
                        edits,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(read_tools(&access, owner, "not-named").await, omitted);
        assert_eq!(
            read_tools(&access, owner, "named")
                .await
                .integrations
                .unwrap()
                .plugins
                .unwrap()[0]
                .digest
                .as_deref(),
            Some(replacement.as_str())
        );
        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stale_tools_refuse_the_pin_transaction_and_preserve_concurrent_authority() {
        use gents_store::identity::NodeIdentity;
        let (_directory, runtime, node, access) = pin_fixture().await;
        let owner = runtime.did();
        let edits = prepare(
            &access,
            owner,
            &["named".into()],
            &BTreeMap::from([("acme/one".into(), format!("sha256:{}", "2".repeat(64)))]),
        )
        .await
        .unwrap();
        let mut concurrent = read_tools(&access, owner, "named").await;
        concurrent.display_name = Some("operator edit".into());
        gents_documents::write_tools_document(
            &access,
            &concurrent,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
        let error = access
            .transact_write("test.pin_moves.stale", |txn| {
                let edits = &edits;
                Box::pin(async move {
                    apply(
                        txn,
                        owner,
                        edits,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("changed after pin preview"));
        assert_eq!(read_tools(&access, owner, "named").await, concurrent);
        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejected_install_transaction_rolls_back_its_pin_edits() {
        use gents_store::identity::NodeIdentity;
        let (_directory, runtime, node, access) = pin_fixture().await;
        let owner = runtime.did();
        let before = read_tools(&access, owner, "named").await;
        let edits = prepare(
            &access,
            owner,
            &["named".into()],
            &BTreeMap::from([("acme/one".into(), format!("sha256:{}", "2".repeat(64)))]),
        )
        .await
        .unwrap();
        access
            .transact_write::<(), anyhow::Error, _>("test.pin_moves.abort", |txn| {
                let edits = &edits;
                Box::pin(async move {
                    apply(
                        txn,
                        owner,
                        edits,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                    anyhow::bail!("install rejected after Tools effects")
                })
            })
            .await
            .expect_err("the install must abort");
        assert_eq!(read_tools(&access, owner, "named").await, before);
        node.shutdown().await.unwrap();
    }
}
