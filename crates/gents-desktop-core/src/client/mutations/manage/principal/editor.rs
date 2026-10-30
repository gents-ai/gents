use anyhow::{ensure, Context, Result};
use gents::config_client::{
    patch::{SelfConfigPatch, SelfConfigTarget},
    read_desired_state_record_in_txn, ConfigAccess, ConfigApplyTxn,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigEditorRevision {
    pub document_id: String,
    pub composite_cid: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfigEditorDocument {
    pub revision: ConfigEditorRevision,
    /// Canonical editable fields encoded natively so integer settings retain precision.
    pub content: String,
}

fn revision_matches(
    expected: Option<&ConfigEditorRevision>,
    current: Option<&ConfigEditorRevision>,
) -> bool {
    matches!((expected, current), (Some(expected), Some(current)) if expected == current)
}

fn editable_fields(target: SelfConfigTarget) -> Vec<&'static str> {
    target
        .writable_fields()
        .into_iter()
        .filter(|field| !(target == SelfConfigTarget::InferenceBackend && *field == "auth"))
        .collect()
}

fn visible(target: SelfConfigTarget, value: &Value) -> Result<Map<String, Value>> {
    let object = value
        .as_object()
        .context("Configuration must be an object.")?;
    Ok(editable_fields(target)
        .into_iter()
        .filter_map(|field| {
            object
                .get(field)
                .map(|value| (field.to_owned(), value.clone()))
        })
        .collect())
}

async fn read_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    target: SelfConfigTarget,
    id: &str,
) -> Result<(ConfigEditorRevision, Value)> {
    let (document_id, value) =
        read_desired_state_record_in_txn(txn, target.collection(), node_did, id)
            .await?
            .context("This configuration no longer exists. Reopen Runtime Settings.")?;
    let composite = gents::graphql::newest_document_composite_commit_in_txn(
        txn,
        &document_id,
        "client.settings.read",
    )
    .await?
    .context("Configuration has no unambiguous composite revision.")?;
    Ok((
        ConfigEditorRevision {
            document_id,
            composite_cid: composite.cid,
        },
        value,
    ))
}

pub async fn read_config_editor_on(
    access: &ConfigAccess,
    node_did: &str,
    target: SelfConfigTarget,
    id: &str,
) -> Result<ConfigEditorDocument> {
    access
        .transact_readonly("client.settings.read", |txn| {
            Box::pin(async move {
                let (revision, value) = read_in_txn(txn, node_did, target, id).await?;
                Ok(ConfigEditorDocument {
                    revision,
                    content: serde_json::to_string_pretty(&visible(target, &value)?)?,
                })
            })
        })
        .await
}

fn adopt_saved_projection(
    read_succeeded: bool,
    physical_matches: bool,
    content_matches: bool,
) -> bool {
    read_succeeded && physical_matches && content_matches
}

pub async fn read_backend_authentication_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<(ConfigEditorRevision, gents::InferenceBackend)> {
    access
        .transact_readonly("client.backend.authentication.read", |txn| {
            Box::pin(async move {
                let (revision, value) =
                    read_in_txn(txn, node_did, SelfConfigTarget::InferenceBackend, id).await?;
                Ok((revision, serde_json::from_value(value)?))
            })
        })
        .await
}

/// The native credential control patches only authentication. It retains the
/// other canonical fields and rejects an edit against an unseen document version.
pub async fn save_backend_authentication_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
    expected: &ConfigEditorRevision,
    auth: &gents::documents::BackendAuth,
) -> Result<()> {
    let patch = vec![("auth".to_owned(), Some(serde_json::to_value(auth)?))];
    access.transact_write("client.backend.authentication.save", |txn| {
        let patch = &patch;
        Box::pin(async move {
            let (current, _) = read_in_txn(txn, node_did, SelfConfigTarget::InferenceBackend, id).await?;
            ensure!(revision_matches(Some(expected), Some(&current)), "Configuration changed since Accounts opened. Reopen Accounts before changing authentication.");
            super::patch_config_components_in_txn(txn, node_did,
                &[(SelfConfigTarget::InferenceBackend, id.to_owned(), patch.clone())]).await
        })
    }).await
}

/// A committed save remains successful when its follow-up observation fails.
/// `None` requires reopening before another edit; a newer, unseen revision must
/// never become the baseline for the editor's existing buffer.
pub async fn save_config_editor_on(
    access: &ConfigAccess,
    node_did: &str,
    target: SelfConfigTarget,
    id: &str,
    expected: &ConfigEditorRevision,
    content: &str,
) -> Result<Option<ConfigEditorDocument>> {
    ensure!(
        content.len() <= 512 * 1024,
        "Configuration exceeds 512 KiB."
    );
    let proposed: Map<String, Value> =
        serde_json::from_str(content).context("Enter a JSON object for configuration.")?;
    let fields = editable_fields(target);
    ensure!(
        proposed.keys().all(|key| fields.contains(&key.as_str())),
        "Only editable configuration fields are allowed. Authentication belongs in Accounts."
    );
    let approved = access.transact_write("client.settings.save", |txn| {
        let proposed = &proposed;
        let fields = &fields;
        Box::pin(async move {
            let (current, retained) = read_in_txn(txn, node_did, target, id).await?;
            ensure!(revision_matches(Some(expected), Some(&current)),
                "Configuration changed since this editor opened. Reopen Runtime Settings and apply your edits to the current version.");
            let retained = visible(target, &retained)?;
            let patch: SelfConfigPatch = fields.iter().filter_map(|field| {
                let candidate = proposed.get(*field);
                (retained.get(*field) != candidate).then(|| ((*field).to_owned(), candidate.cloned()))
            }).collect();
            super::patch_config_components_in_txn(txn, node_did, &[(target, id.to_owned(), patch)]).await?;
            let (_, canonical) = read_desired_state_record_in_txn(txn, target.collection(), node_did, id)
                .await?.context("Saved configuration disappeared in its transaction.")?;
            visible(target, &canonical)
        })
    }).await?;
    match read_config_editor_on(access, node_did, target, id).await {
        Ok(document) => {
            let content_matches = serde_json::from_str::<Map<String, Value>>(&document.content)
                .is_ok_and(|observed| observed == approved);
            Ok(adopt_saved_projection(
                true,
                document.revision.document_id == expected.document_id,
                content_matches,
            )
            .then_some(document))
        }
        Err(_) => {
            tracing::warn!("Configuration saved; its updated editor projection is unavailable");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::config_client::{apply_desired_state_plan, DesiredStateApplyPlan};
    use serde_json::json;
    use std::sync::Arc;

    async fn fixture() -> Result<ConfigAccess> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let config = serde_json::from_value(json!({
            "node": {"node_did":"did:test:editor"},
            "inference_backends": [{"node_did":"did:test:editor","backend_id":"backend","name":"Original",
                "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
                "auth":{"kind":"api_key","key":"private-test-credential"}}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        let access = ConfigAccess::local(node);
        access
            .transact_write("test.editor.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        Ok(access)
    }

    #[tokio::test]
    async fn config_editor_saves_canonical_fields_and_retains_private_authentication() -> Result<()>
    {
        let access = fixture().await?;
        let original = read_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        assert!(!original.content.contains("private-test-credential"));
        let mut content: Map<String, Value> = serde_json::from_str(&original.content)?;
        assert!(!content.contains_key("auth"));
        assert!(!content.contains_key("node_did"));
        content.insert("name".into(), json!("Updated"));
        let saved = save_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
            &original.revision,
            &serde_json::to_string(&content)?,
        )
        .await?
        .context("saved projection")?;
        assert_eq!(saved.revision.document_id, original.revision.document_id);
        assert_ne!(
            saved.revision.composite_cid,
            original.revision.composite_cid
        );
        access
            .transact_readonly("test.editor.verify", |txn| {
                Box::pin(async move {
                    let (_, value) = read_in_txn(
                        txn,
                        "did:test:editor",
                        SelfConfigTarget::InferenceBackend,
                        "backend",
                    )
                    .await?;
                    assert_eq!(value["name"], "Updated");
                    assert_eq!(value["auth"]["key"], "private-test-credential");
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let mut private = content.clone();
        private.insert("auth".into(), json!({"kind":"unauthenticated"}));
        assert!(save_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
            &saved.revision,
            &serde_json::to_string(&private)?
        )
        .await
        .is_err());
        let after = read_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        assert_eq!(saved.revision, after.revision);
        Ok(())
    }

    #[tokio::test]
    async fn config_editor_allows_only_one_writer_from_the_same_revision() -> Result<()> {
        let access = fixture().await?;
        let original = read_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        let first = original.content.replace("Original", "First editor");
        let second = original.content.replace("Original", "Second editor");
        let (first, second) = tokio::join!(
            save_config_editor_on(
                &access,
                "did:test:editor",
                SelfConfigTarget::InferenceBackend,
                "backend",
                &original.revision,
                &first
            ),
            save_config_editor_on(
                &access,
                "did:test:editor",
                SelfConfigTarget::InferenceBackend,
                "backend",
                &original.revision,
                &second
            ),
        );
        assert_ne!(first.is_ok(), second.is_ok(), "exactly one editor may save");
        let conflict = first.err().or_else(|| second.err()).unwrap();
        assert!(
            conflict.to_string().contains("Configuration changed"),
            "{conflict:#}"
        );
        let current = read_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        assert_ne!(current.revision, original.revision);
        Ok(())
    }

    #[tokio::test]
    async fn config_editor_native_authentication_rejects_stale_edits_and_retains_public_settings(
    ) -> Result<()> {
        let access = fixture().await?;
        let (revision, original) =
            read_backend_authentication_on(&access, "did:test:editor", "backend").await?;
        let replacement = gents::documents::BackendAuth::ApiKey {
            key: "replacement-private-key".into(),
        };
        save_backend_authentication_on(
            &access,
            "did:test:editor",
            "backend",
            &revision,
            &replacement,
        )
        .await?;
        let (current, saved) =
            read_backend_authentication_on(&access, "did:test:editor", "backend").await?;
        assert_eq!(saved.auth, replacement);
        assert_eq!(saved.endpoint, original.endpoint);
        assert_eq!(saved.name, original.name);
        assert!(save_backend_authentication_on(
            &access,
            "did:test:editor",
            "backend",
            &revision,
            &gents::documents::BackendAuth::Unauthenticated
        )
        .await
        .is_err());
        let (unchanged, saved) =
            read_backend_authentication_on(&access, "did:test:editor", "backend").await?;
        assert_eq!(current, unchanged);
        assert_eq!(saved.auth, replacement);
        let public = read_config_editor_on(
            &access,
            "did:test:editor",
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        assert!(!public.content.contains("replacement-private-key"));
        Ok(())
    }

    #[test]
    fn config_editor_matches_executable_lean_contract() {
        let contract: Value =
            gents_lean_contract::load_contract_snapshot().expect("Lean settings contract");
        let cases = &contract["client_settings_cases"];
        for case in cases["revisions"].as_array().unwrap() {
            let decode = |value: &Value| {
                (!value.is_null()).then(|| ConfigEditorRevision {
                    document_id: value["document"].to_string(),
                    composite_cid: value["composite"].to_string(),
                })
            };
            assert_eq!(
                revision_matches(
                    decode(&case["expected"]).as_ref(),
                    decode(&case["current"]).as_ref()
                ),
                case["matches"].as_bool().unwrap()
            );
        }
        for case in cases["fields"].as_array().unwrap() {
            let target =
                SelfConfigTarget::from_collection_name(case["collection"].as_str().unwrap())
                    .unwrap();
            assert_eq!(
                serde_json::to_value(editable_fields(target)).unwrap(),
                case["editable"]
            );
        }
        for case in cases["savedProjections"].as_array().unwrap() {
            assert_eq!(
                adopt_saved_projection(
                    case["read"].as_bool().unwrap(),
                    case["physical"].as_bool().unwrap(),
                    case["content"].as_bool().unwrap()
                ),
                case["adopt"].as_bool().unwrap()
            );
        }
    }
}
