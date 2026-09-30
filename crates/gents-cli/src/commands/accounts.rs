//! `gents accounts`: the subscription accounts signed in on this node, and the
//! backends that use no account, with the profiles that use each.

use std::io::IsTerminal;

use anyhow::Result;
use gents::oauth_credential::AccountSummary;
use serde::Serialize;
use serde_json::Value;

use crate::cli::args::{AccountsCommand, AccountsTargetArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_agent_did, resolve_config_access};

/// One `accounts list` row: a sign-in account, a backend that uses no
/// account, or a subscription backend whose account is not on this node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AccountRow {
    pub(crate) provider: String,
    pub(crate) label: String,
    pub(crate) identity: Option<String>,
    pub(crate) plan: Option<String>,
    pub(crate) status: String,
    pub(crate) default: bool,
    pub(crate) credential_id: Option<String>,
    pub(crate) account_ref: Option<String>,
    pub(crate) backend_id: Option<String>,
    pub(crate) profiles: Vec<String>,
}

pub(crate) async fn dispatch(command: AccountsCommand) -> Result<()> {
    match command {
        AccountsCommand::List { target, output } => {
            let (access, did) = target_access(&target).await?;
            let rows = account_rows(&access, &did, target.provider.as_deref()).await?;
            match output
                .ensure_supported("accounts list", &[OutputFormat::Table, OutputFormat::Json])?
            {
                OutputFormat::Json => print_json(&serde_json::to_value(rows)?),
                _ => {
                    print!("{}", render_table(&rows));
                    Ok(())
                }
            }
        }
        AccountsCommand::Label {
            target,
            account,
            label,
        } => {
            let (access, did) = target_access(&target).await?;
            print_json(
                &label_account(&access, &did, &account, target.provider.as_deref(), &label).await?,
            )
        }
        AccountsCommand::Disable { target, account } => {
            let (access, did) = target_access(&target).await?;
            let result =
                disable_account(&access, &did, &account, target.provider.as_deref()).await?;
            print_json(&result)
        }
        AccountsCommand::Remove {
            target,
            account,
            yes,
        } => {
            let (access, did) = target_access(&target).await?;
            let confirmed = yes
                || (std::io::stdin().is_terminal()
                    && std::io::stderr().is_terminal()
                    && crate::interactive_backend::confirm(
                        &format!("Remove account {account:?} from this node?"),
                        false,
                    )
                    .await);
            let result = remove_account(
                &access,
                &did,
                &account,
                target.provider.as_deref(),
                confirmed,
            )
            .await?;
            print_json(&result)
        }
    }
}

async fn target_access(target: &AccountsTargetArgs) -> Result<(crate::CommandAccess, String)> {
    let (access, home_dir) =
        resolve_config_access(target.home.as_deref(), target.graphql.as_deref()).await?;
    let did = resolve_agent_did(Some(&home_dir), target.agent_did.as_deref())?;
    Ok((access, did))
}

pub(crate) async fn account_rows(
    _access: &ConfigAccess,
    _agent_did: &str,
    _provider: Option<&str>,
) -> Result<Vec<AccountRow>> {
    anyhow::bail!("not implemented")
}

pub(crate) fn render_table(_rows: &[AccountRow]) -> String {
    String::new()
}

pub(crate) async fn resolve_account(
    _access: &ConfigAccess,
    _agent_did: &str,
    _account: &str,
    _provider: Option<&str>,
) -> Result<AccountSummary> {
    anyhow::bail!("not implemented")
}

pub(crate) async fn label_account(
    _access: &ConfigAccess,
    _agent_did: &str,
    _account: &str,
    _provider: Option<&str>,
    _label: &str,
) -> Result<Value> {
    anyhow::bail!("not implemented")
}

pub(crate) async fn disable_account(
    _access: &ConfigAccess,
    _agent_did: &str,
    _account: &str,
    _provider: Option<&str>,
) -> Result<Value> {
    anyhow::bail!("not implemented")
}

pub(crate) async fn remove_account(
    _access: &ConfigAccess,
    _agent_did: &str,
    _account: &str,
    _provider: Option<&str>,
    _confirmed: bool,
) -> Result<Value> {
    anyhow::bail!("not implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::document_config::BackendAuth;
    use gents::oauth_credential::{list_oauth_credentials_on, upsert_oauth_credential_on};
    use gents::{BackendProviderKind, InferenceBackend};
    use serde_json::json;
    use std::sync::Arc;

    const DID: &str = "did:key:z6MkTestAccounts";
    const CHATGPT: &str = "chatgpt-codex";
    const CLAUDE: &str = "claude-subscription";
    const GROK: &str = "xai-oauth";

    async fn seed_account(
        access: &ConfigAccess,
        provider: &str,
        account_ref: Option<&str>,
        label: Option<&str>,
    ) {
        let mut credential = gents::claude_oauth::credential_from_login_tokens(
            DID,
            provider,
            &gents::claude_oauth::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: "refresh-SECRET".into(),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("identity-{}", account_ref.unwrap_or("original"))),
                organization_uuid: None,
                account_uuid: None,
            },
            chrono::Utc::now(),
        );
        if let Some(account_ref) = account_ref {
            credential.credential_id = format!("{provider}:{DID}:{account_ref}");
            credential.account_ref = Some(account_ref.to_owned());
            credential.connected_at = Some(chrono::Utc::now());
        }
        credential.label = label.map(str::to_owned);
        upsert_oauth_credential_on(access, &credential)
            .await
            .unwrap();
    }

    fn backend(
        backend_id: &str,
        name: &str,
        provider_kind: BackendProviderKind,
        auth: BackendAuth,
    ) -> InferenceBackend {
        let mut value = json!({
            "agent_did": DID,
            "backend_id": backend_id,
            "name": name,
            "provider_kind": provider_kind,
            "endpoint": "http://127.0.0.1:1/v1",
            "auth": auth,
        });
        if provider_kind == BackendProviderKind::ClaudeCliSubscription {
            value["endpoint"] = json!("claude-cli://subscription");
        }
        serde_json::from_value(value).unwrap()
    }

    async fn seed_backend(access: &ConfigAccess, backend: InferenceBackend) {
        gents::config_client::write_inference_backend_document(access, &backend)
            .await
            .unwrap();
    }

    async fn seed_profile(access: &ConfigAccess, profile_id: &str, backend_id: &str) {
        let profile = serde_json::from_value(json!({
            "agent_did": DID,
            "profile_id": profile_id,
            "backend_id": backend_id,
            "model_name": "model-x",
        }))
        .unwrap();
        gents::config_client::write_inference_profile_document(access, &profile)
            .await
            .unwrap();
    }

    fn oauth(account_ref: Option<&str>) -> BackendAuth {
        BackendAuth::PrincipalOAuth {
            account_ref: account_ref.map(str::to_owned),
        }
    }

    /// Two accounts per sign-in provider, two API-key and two endpoint
    /// backends, and profiles on some of them.
    async fn seeded() -> ConfigAccess {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node);
        seed_account(&access, CHATGPT, None, None).await;
        seed_account(&access, CHATGPT, Some("acct-c2"), Some("ChatGPT 2")).await;
        seed_account(&access, CLAUDE, None, Some("Personal")).await;
        seed_account(&access, CLAUDE, Some("acct-l2"), Some("Work")).await;
        seed_account(&access, GROK, None, None).await;
        seed_account(&access, GROK, Some("acct-g2"), Some("Grok 2")).await;
        use BackendProviderKind::*;
        for backend in [
            backend("claude", "Claude", ClaudeCliSubscription, oauth(None)),
            backend(
                "claude-acct-l2",
                "Work",
                ClaudeCliSubscription,
                oauth(Some("acct-l2")),
            ),
            backend(
                "chatgpt-acct-c2",
                "ChatGPT 2",
                ChatGptCodex,
                oauth(Some("acct-c2")),
            ),
            backend(
                "openai",
                "OpenAI",
                OpenAiCompatible,
                BackendAuth::ApiKey {
                    key: "key-SECRET".into(),
                },
            ),
            backend(
                "openrouter",
                "OpenRouter",
                OpenRouter,
                BackendAuth::ApiKey {
                    key: "key-SECRET".into(),
                },
            ),
            backend(
                "local",
                "Local server",
                OpenAiCompatible,
                BackendAuth::Unauthenticated,
            ),
            backend(
                "lab",
                "Lab server",
                OpenAiCompatible,
                BackendAuth::Unauthenticated,
            ),
        ] {
            seed_backend(&access, backend).await;
        }
        seed_profile(&access, "default-profile", "claude").await;
        seed_profile(&access, "work-profile", "claude-acct-l2").await;
        seed_profile(&access, "api-profile", "openai").await;
        access
    }

    fn row<'a>(rows: &'a [AccountRow], label: &str) -> &'a AccountRow {
        rows.iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("no row {label}: {rows:#?}"))
    }

    async fn stored_ids(access: &ConfigAccess) -> Vec<String> {
        list_oauth_credentials_on(access, DID)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.credential_id)
            .collect()
    }

    async fn backend_ids(access: &ConfigAccess) -> Vec<String> {
        let ConfigAccess::Local(node) = access else {
            unreachable!()
        };
        gents::backend_registry::list_all_backends(node)
            .await
            .unwrap()
            .into_iter()
            .map(|backend| backend.backend_id)
            .collect()
    }

    #[tokio::test]
    async fn list_shows_every_account_and_backend() {
        let access = seeded().await;
        let rows = account_rows(&access, DID, None).await.unwrap();
        let mut labels: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
        labels.sort();
        assert_eq!(
            labels,
            [
                "ChatGPT",
                "ChatGPT 2",
                "Grok",
                "Grok 2",
                "Lab server",
                "Local server",
                "OpenAI",
                "OpenRouter",
                "Personal",
                "Work"
            ]
        );
        assert_eq!(row(&rows, "Personal").profiles, ["default-profile"]);
        assert_eq!(row(&rows, "Work").profiles, ["work-profile"]);
        assert_eq!(row(&rows, "OpenAI").profiles, ["api-profile"]);
        assert!(row(&rows, "ChatGPT 2").profiles.is_empty());
        let defaults: Vec<_> = rows
            .iter()
            .filter(|row| row.default)
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(defaults.len(), 3);
        for label in ["ChatGPT", "Personal", "Grok"] {
            assert!(defaults.contains(&label), "{defaults:?}");
        }
        assert_eq!(
            row(&rows, "Personal").identity.as_deref(),
            Some("identity-original")
        );

        let json = serde_json::to_string(&rows).unwrap();
        let table = render_table(&rows);
        for text in [&json, &table] {
            assert!(!text.contains("SECRET"), "{text}");
        }
        for heading in [
            "PROVIDER", "LABEL", "IDENTITY", "PLAN", "STATUS", "DEFAULT", "PROFILES",
        ] {
            assert!(table.contains(heading), "{table}");
        }
        assert!(
            table.contains("work-profile") && table.contains("Lab server"),
            "{table}"
        );

        let claude_only = account_rows(&access, DID, Some(CLAUDE)).await.unwrap();
        let mut labels: Vec<_> = claude_only.iter().map(|row| row.label.as_str()).collect();
        labels.sort();
        assert_eq!(labels, ["Personal", "Work"]);
    }

    #[tokio::test]
    async fn a_backend_whose_account_is_elsewhere_is_listed_as_missing() {
        let access = seeded().await;
        seed_backend(
            &access,
            backend(
                "grok-remote",
                "Remote Grok",
                BackendProviderKind::XaiGrokOAuth,
                oauth(Some("acct-other")),
            ),
        )
        .await;
        let rows = account_rows(&access, DID, None).await.unwrap();
        assert_eq!(rows.len(), 11);
        let missing = row(&rows, "Remote Grok");
        assert_eq!(missing.status, "account not on this node");
        assert_eq!(missing.backend_id.as_deref(), Some("grok-remote"));
        assert!(render_table(&rows).contains("account not on this node"));
    }

    #[tokio::test]
    async fn a_shared_label_is_ambiguous_until_narrowed() {
        let access = seeded().await;
        gents::oauth_credential::set_account_label(
            &access,
            DID,
            &format!("{GROK}:{DID}:acct-g2"),
            "Work",
        )
        .await
        .unwrap();
        let error = resolve_account(&access, DID, "Work", None)
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.contains(&format!("{CLAUDE}:{DID}:acct-l2")), "{text}");
        assert!(text.contains(&format!("{GROK}:{DID}:acct-g2")), "{text}");
        assert!(text.contains("--provider"), "{text}");
        let by_provider = resolve_account(&access, DID, "Work", Some(CLAUDE))
            .await
            .unwrap();
        assert_eq!(by_provider.account_ref.as_deref(), Some("acct-l2"));
        let by_id = resolve_account(&access, DID, &format!("{GROK}:{DID}:acct-g2"), None)
            .await
            .unwrap();
        assert_eq!(by_id.provider, GROK);
        let by_ref = resolve_account(&access, DID, "acct-g2", None)
            .await
            .unwrap();
        assert_eq!(by_ref.provider, GROK);

        // Two accounts of one provider with one label (concurrent adds).
        seed_account(&access, CHATGPT, Some("acct-c3"), Some("ChatGPT 2")).await;
        let error = resolve_account(&access, DID, "ChatGPT 2", Some(CHATGPT))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("acct-c3"), "{error}");
        assert!(resolve_account(&access, DID, "acct-c3", None).await.is_ok());
    }

    #[tokio::test]
    async fn a_backend_label_points_to_config_backend() {
        let access = seeded().await;
        let error = resolve_account(&access, DID, "OpenAI", None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("gents config backend"),
            "{error}"
        );
        let error = resolve_account(&access, DID, "nothing-here", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nothing-here"), "{error}");
    }

    #[tokio::test]
    async fn labels_rename_and_refuse_duplicates() {
        let access = seeded().await;
        let result = label_account(&access, DID, "Work", None, "Office")
            .await
            .unwrap();
        assert_eq!(result["label"], "Office");
        let rows = account_rows(&access, DID, None).await.unwrap();
        assert_eq!(row(&rows, "Office").profiles, ["work-profile"]);
        let ConfigAccess::Local(node) = &access else {
            unreachable!()
        };
        let renamed = gents::backend_registry::lookup_backend(node, DID, "claude-acct-l2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(renamed.name, "Office");
        assert!(label_account(&access, DID, "Office", None, "Personal")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn disable_names_the_profiles_and_keeps_the_row() {
        let access = seeded().await;
        let result = disable_account(&access, DID, "Work", None).await.unwrap();
        assert_eq!(result["profiles"], json!(["work-profile"]));
        let rows = account_rows(&access, DID, None).await.unwrap();
        assert_eq!(row(&rows, "Work").status, "disabled");
        assert!(stored_ids(&access)
            .await
            .contains(&format!("{CLAUDE}:{DID}:acct-l2")));
    }

    #[tokio::test]
    async fn remove_deletes_the_row_and_an_unnamed_backend_but_keeps_a_named_one() {
        let access = seeded().await;
        let result = remove_account(&access, DID, "ChatGPT 2", None, true)
            .await
            .unwrap();
        assert_eq!(result["deleted_backends"], json!(["chatgpt-acct-c2"]));
        assert!(!stored_ids(&access)
            .await
            .contains(&format!("{CHATGPT}:{DID}:acct-c2")));
        assert!(!backend_ids(&access)
            .await
            .contains(&"chatgpt-acct-c2".to_string()));

        let result = remove_account(&access, DID, "Work", None, true)
            .await
            .unwrap();
        assert_eq!(result["profiles"], json!(["work-profile"]));
        assert_eq!(result["kept_backends"], json!(["claude-acct-l2"]));
        assert!(!stored_ids(&access)
            .await
            .contains(&format!("{CLAUDE}:{DID}:acct-l2")));
        assert!(backend_ids(&access)
            .await
            .contains(&"claude-acct-l2".to_string()));
    }

    #[tokio::test]
    async fn removing_the_original_keeps_the_no_reference_backend() {
        let access = seeded().await;
        remove_account(&access, DID, "Personal", None, true)
            .await
            .unwrap();
        assert!(!stored_ids(&access)
            .await
            .contains(&format!("{CLAUDE}:{DID}")));
        assert!(backend_ids(&access).await.contains(&"claude".to_string()));
    }

    #[tokio::test]
    async fn remove_off_a_terminal_needs_yes() {
        let access = seeded().await;
        let error = remove_account(&access, DID, "Grok 2", None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--yes"), "{error}");
        assert!(stored_ids(&access)
            .await
            .contains(&format!("{GROK}:{DID}:acct-g2")));
    }
}
