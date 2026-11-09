use anyhow::{ensure, Context, Result};
use gents::inference::backends::rotation::{
    backend_pool_members, set_backend_rotation_in_txn, BackendRotation, RotationOutcome,
};
use gents::store::config_client::{
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
    let composite = gents::store::graphql::newest_document_composite_commit_in_txn(
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

const SETTINGS_CHANGED: &str = "Configuration changed since this editor opened. Reopen Runtime Settings and apply your edits to the current version.";
const ACCOUNTS_AUTH_CHANGED: &str =
    "Configuration changed since Accounts opened. Reopen Accounts before changing authentication.";
const ACCOUNTS_POOL_CHANGED: &str = "Configuration changed since Accounts opened. Reopen Accounts before changing the account pool.";

/// A revision-guarded save that loses a concurrent commit is the same stale
/// edit as one whose revision check failed. A dispatched client-signed write
/// is never replayed after a conflict, so over the User's HTTP endpoint the
/// loser sees the native conflict rather than a retried revision check.
fn stale_on_conflict<T>(result: Result<T>, message: &'static str) -> Result<T> {
    use gents::store::config_client::{ConfigClientError, TransactionFailure as _};
    result.map_err(|error| {
        if matches!(
            error.config_client_error(),
            Some(ConfigClientError::TransactionConflict(_))
        ) {
            error.context(message)
        } else {
            error
        }
    })
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
    stale_on_conflict(
        access
            .transact_write("client.backend.authentication.save", |txn| {
                let patch = &patch;
                Box::pin(async move {
                    let (current, _) =
                        read_in_txn(txn, node_did, SelfConfigTarget::InferenceBackend, id).await?;
                    ensure!(
                        revision_matches(Some(expected), Some(&current)),
                        ACCOUNTS_AUTH_CHANGED
                    );
                    super::patch_config_components_in_txn(
                        txn,
                        node_did,
                        &[(
                            SelfConfigTarget::InferenceBackend,
                            id.to_owned(),
                            patch.clone(),
                        )],
                    )
                    .await
                })
            })
            .await,
        ACCOUNTS_AUTH_CHANGED,
    )
}

/// Switches a subscription backend between one account and the account pool
/// through the shared operator write. A caller holding a revision gets the same
/// stale-edit refusal as the authentication control.
pub async fn set_backend_rotation_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
    expected: Option<&ConfigEditorRevision>,
    rotation: BackendRotation,
) -> Result<RotationOutcome> {
    let saved = access
        .transact_write("client.backend.rotation.save", |txn| {
            Box::pin(async move {
                if let Some(expected) = expected {
                    let (current, _) =
                        read_in_txn(txn, node_did, SelfConfigTarget::InferenceBackend, id).await?;
                    ensure!(
                        revision_matches(Some(expected), Some(&current)),
                        ACCOUNTS_POOL_CHANGED
                    );
                }
                set_backend_rotation_in_txn(txn, node_did, id, rotation).await
            })
        })
        .await;
    let backend = if expected.is_some() {
        stale_on_conflict(saved, ACCOUNTS_POOL_CHANGED)?
    } else {
        saved?
    };
    let members = backend_pool_members(access, &backend).await?;
    Ok(RotationOutcome { backend, members })
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
    let approved = stale_on_conflict(
        access
            .transact_write("client.settings.save", |txn| {
                let proposed = &proposed;
                let fields = &fields;
                Box::pin(async move {
                    let (current, retained) = read_in_txn(txn, node_did, target, id).await?;
                    ensure!(
                        revision_matches(Some(expected), Some(&current)),
                        SETTINGS_CHANGED
                    );
                    let retained = visible(target, &retained)?;
                    let patch: SelfConfigPatch = fields
                        .iter()
                        .filter_map(|field| {
                            let candidate = proposed.get(*field);
                            (retained.get(*field) != candidate)
                                .then(|| ((*field).to_owned(), candidate.cloned()))
                        })
                        .collect();
                    super::patch_config_components_in_txn(
                        txn,
                        node_did,
                        &[(target, id.to_owned(), patch)],
                    )
                    .await?;
                    let (_, canonical) =
                        read_desired_state_record_in_txn(txn, target.collection(), node_did, id)
                            .await?
                            .context("Saved configuration disappeared in its transaction.")?;
                    visible(target, &canonical)
                })
            })
            .await,
        SETTINGS_CHANGED,
    )?;
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
    use crate::test_fixtures::ServedRuntimeHome;
    use gents::documents::apply_desired_state_plan;
    use gents::store::config_client::DesiredStateApplyPlan;
    use serde_json::json;

    /// The runtime and the User's access to it, with one API-key backend.
    async fn fixture() -> Result<(ServedRuntimeHome, ConfigAccess)> {
        let runtime = ServedRuntimeHome::start().await;
        let editor = runtime.node_did();
        let config = serde_json::from_value(json!({
            "node": {"node_did":editor},
            "inference_backends": [{"node_did":editor,"backend_id":"backend","name":"Original",
                "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
                "auth":{"kind":"api_key","key":"private-test-credential"}}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        let access = runtime.user_access();
        access
            .transact_write("test.editor.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        Ok((runtime, access))
    }

    #[tokio::test]
    async fn config_editor_saves_canonical_fields_and_retains_private_authentication() -> Result<()>
    {
        let (runtime, access) = fixture().await?;
        let editor = runtime.node_did();
        let original = read_config_editor_on(
            &access,
            editor,
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
            editor,
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
                    let (_, value) =
                        read_in_txn(txn, editor, SelfConfigTarget::InferenceBackend, "backend")
                            .await?;
                    assert_eq!(value["name"], "Updated");
                    assert_eq!(value["auth"]["kind"], "credential");
                    assert!(value["auth"].get("key").is_none());
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let mut private = content.clone();
        private.insert("auth".into(), json!({"kind":"unauthenticated"}));
        assert!(save_config_editor_on(
            &access,
            editor,
            SelfConfigTarget::InferenceBackend,
            "backend",
            &saved.revision,
            &serde_json::to_string(&private)?
        )
        .await
        .is_err());
        let after = read_config_editor_on(
            &access,
            editor,
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        assert_eq!(saved.revision, after.revision);
        Ok(())
    }

    #[tokio::test]
    async fn config_editor_allows_only_one_writer_from_the_same_revision() -> Result<()> {
        let (runtime, access) = fixture().await?;
        let editor = runtime.node_did();
        let original = read_config_editor_on(
            &access,
            editor,
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        let first = original.content.replace("Original", "First editor");
        let second = original.content.replace("Original", "Second editor");
        let (first, second) = tokio::join!(
            save_config_editor_on(
                &access,
                editor,
                SelfConfigTarget::InferenceBackend,
                "backend",
                &original.revision,
                &first
            ),
            save_config_editor_on(
                &access,
                editor,
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
            editor,
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
        let (runtime, access) = fixture().await?;
        let editor = runtime.node_did();
        let (revision, original) =
            read_backend_authentication_on(&access, editor, "backend").await?;
        let replacement = gents::documents::BackendAuth::ApiKey {
            key: "replacement-private-key".into(),
        };
        save_backend_authentication_on(&access, editor, "backend", &revision, &replacement).await?;
        let (current, saved) = read_backend_authentication_on(&access, editor, "backend").await?;
        assert!(matches!(
            saved.auth,
            gents::documents::BackendAuth::Credential { .. }
        ));
        let stored = saved.auth.clone();
        assert_ne!(
            stored, original.auth,
            "the replacement is a new credential version"
        );
        assert_eq!(saved.endpoint, original.endpoint);
        assert_eq!(saved.name, original.name);
        assert!(save_backend_authentication_on(
            &access,
            editor,
            "backend",
            &revision,
            &gents::documents::BackendAuth::Unauthenticated
        )
        .await
        .is_err());
        let (unchanged, saved) = read_backend_authentication_on(&access, editor, "backend").await?;
        assert_eq!(current, unchanged);
        // The rejected edit leaves the credential that stores the replacement.
        assert_eq!(saved.auth, stored);
        let public = read_config_editor_on(
            &access,
            editor,
            SelfConfigTarget::InferenceBackend,
            "backend",
        )
        .await?;
        assert!(!public.content.contains("replacement-private-key"));
        Ok(())
    }

    async fn claude_backend(access: &ConfigAccess, editor: &str) -> Result<()> {
        let backend: gents::InferenceBackend = serde_json::from_value(json!({
            "node_did":editor,"backend_id":"claude","name":"Claude",
            "provider_kind":"ClaudeCliSubscription","endpoint":"claude-cli://subscription",
            "auth":{"kind":"node_oauth"}
        }))?;
        gents::documents::write_inference_backend_document(access, &backend).await?;
        Ok(())
    }

    #[tokio::test]
    async fn backend_rotation_round_trips_and_keeps_the_other_fields() -> Result<()> {
        let (runtime, access) = fixture().await?;
        let editor = runtime.node_did();
        claude_backend(&access, editor).await?;
        let did = editor;
        let (revision, before) = read_backend_authentication_on(&access, did, "claude").await?;
        let pool = set_backend_rotation_on(
            &access,
            did,
            "claude",
            Some(&revision),
            BackendRotation::Pool {
                allow_overage: Some(true),
            },
        )
        .await?;
        assert_eq!(
            pool.backend.auth,
            gents::documents::BackendAuth::NodeOAuthPool {
                allow_overage: true
            }
        );
        let (revision, stored) = read_backend_authentication_on(&access, did, "claude").await?;
        assert_eq!(stored.auth, pool.backend.auth);
        assert_eq!(stored.endpoint, before.endpoint);
        assert_eq!(stored.name, before.name);
        let kept = set_backend_rotation_on(
            &access,
            did,
            "claude",
            Some(&revision),
            BackendRotation::Pool {
                allow_overage: None,
            },
        )
        .await?;
        assert_eq!(kept.backend.auth.pool_allow_overage(), Some(true));
        let (revision, _) = read_backend_authentication_on(&access, did, "claude").await?;
        let single = set_backend_rotation_on(
            &access,
            did,
            "claude",
            Some(&revision),
            BackendRotation::Single,
        )
        .await?;
        assert_eq!(
            single.backend.auth,
            gents::documents::BackendAuth::NodeOAuth { account_ref: None }
        );
        assert!(single.members.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn backend_rotation_refuses_a_stale_revision_and_changes_nothing() -> Result<()> {
        let (runtime, access) = fixture().await?;
        let editor = runtime.node_did();
        claude_backend(&access, editor).await?;
        let did = editor;
        let (stale, _) = read_backend_authentication_on(&access, did, "claude").await?;
        set_backend_rotation_on(
            &access,
            did,
            "claude",
            None,
            BackendRotation::Pool {
                allow_overage: None,
            },
        )
        .await?;
        let error = set_backend_rotation_on(
            &access,
            did,
            "claude",
            Some(&stale),
            BackendRotation::Single,
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("Configuration changed"),
            "{error:#}"
        );
        let (_, stored) = read_backend_authentication_on(&access, did, "claude").await?;
        assert_eq!(stored.auth.pool_allow_overage(), Some(false));
        Ok(())
    }

    #[tokio::test]
    async fn backend_rotation_refuses_a_backend_that_cannot_rotate() -> Result<()> {
        let (runtime, access) = fixture().await?;
        let editor = runtime.node_did();
        let did = editor;
        let (revision, _) = read_backend_authentication_on(&access, did, "backend").await?;
        let error = set_backend_rotation_on(
            &access,
            did,
            "backend",
            Some(&revision),
            BackendRotation::Pool {
                allow_overage: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("subscription"), "{error:#}");
        let (after, stored) = read_backend_authentication_on(&access, did, "backend").await?;
        assert_eq!(after, revision);
        assert!(matches!(
            stored.auth,
            gents::documents::BackendAuth::Credential { .. }
        ));
        let missing =
            set_backend_rotation_on(&access, did, "absent", None, BackendRotation::Single)
                .await
                .unwrap_err();
        assert!(
            missing.to_string().contains("does not exist"),
            "{missing:#}"
        );
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
