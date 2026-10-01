use crate::cli::output_format::OutputFormat;
use crate::cli::{
    ConfigListArgs, ConfigShowArgs, InferenceProfileSetAccountArgs, InferenceProfileSetArgs,
};
use crate::commands::accounts::{snapshot, Snapshot};
use crate::config_writes::ConfigAccess;
use anyhow::{Context, Result};
use gents::document_config::InferenceProfile;
use gents::oauth_credential::{
    backend_account, serving_account, AccountState, AccountSummary, ServingAccount,
};
use serde_json::{json, Value};

fn decode_profile(contents: &[u8]) -> Result<InferenceProfile> {
    let profile: InferenceProfile =
        serde_json::from_slice(contents).context("decoding canonical InferenceProfile document")?;
    profile.validate()?;
    Ok(profile)
}

pub(super) async fn inference_profile_set(args: InferenceProfileSetArgs) -> Result<()> {
    let contents =
        std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    crate::print_json(
        &set_profile(
            &access,
            &contents,
            args.account.as_deref(),
            args.provider.as_deref(),
        )
        .await?,
    )
}

pub(super) async fn profile_set_account(args: InferenceProfileSetAccountArgs) -> Result<()> {
    let (access, agent_did) = target(args.home.as_deref(), args.graphql.as_deref()).await?;
    crate::print_json(
        &set_account(
            &access,
            &agent_did,
            &args.profile,
            &args.account,
            args.provider.as_deref(),
        )
        .await?,
    )
}

/// Write the profile `contents` holds. `account` (narrowed by `provider`), or
/// `provider`'s default account when only `provider` is given, fills the
/// profile's `backend_id`.
async fn set_profile(
    access: &ConfigAccess,
    contents: &[u8],
    account: Option<&str>,
    provider: Option<&str>,
) -> Result<Value> {
    if account.is_none() && provider.is_none() {
        let profile = decode_profile(contents)?;
        let snapshot = snapshot(access, &profile.agent_did).await?;
        return write_checked(access, &snapshot, &profile, false).await;
    }
    let mut value: Value =
        serde_json::from_slice(contents).context("decoding canonical InferenceProfile document")?;
    let agent_did = value["agent_did"]
        .as_str()
        .context("the profile document names no agent_did")?
        .to_owned();
    let snapshot = snapshot(access, &agent_did).await?;
    let summary = match (account, provider) {
        (Some(account), provider) => snapshot.pick(account, provider)?,
        (None, Some(provider)) => snapshot
            .accounts
            .iter()
            .find(|account| account.provider == provider && account.default)
            .with_context(|| {
                gents::oauth_credential::enabled_accounts_note(provider, &snapshot.accounts)
            })?,
        (None, None) => unreachable!("handled above"),
    };
    let backend_id = account_backend(&snapshot, summary)?;
    match value["backend_id"].as_str() {
        Some(named) if named != backend_id => anyhow::bail!(
            "the file names backend {named:?}, but account {:?} runs on {backend_id:?}; \
             drop backend_id from the file or the account flags",
            summary.label
        ),
        _ => value["backend_id"] = json!(backend_id),
    }
    let profile = decode_profile(&serde_json::to_vec(&value)?)?;
    write_checked(access, &snapshot, &profile, account.is_none()).await
}

/// Move `profile_id` to `account`'s backend; nothing else changes.
async fn set_account(
    access: &ConfigAccess,
    agent_did: &str,
    profile_id: &str,
    account: &str,
    provider: Option<&str>,
) -> Result<Value> {
    let snapshot = snapshot(access, agent_did).await?;
    let mut profile = snapshot
        .profiles
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .with_context(|| {
            format!("no profile {profile_id:?}; `gents config profile list` shows them")
        })?
        .clone();
    profile.backend_id = account_backend(&snapshot, snapshot.pick(account, provider)?)?;
    write_checked(access, &snapshot, &profile, false).await
}

/// The one backend that runs on `account`.
fn account_backend(snapshot: &Snapshot, account: &AccountSummary) -> Result<String> {
    let backends: Vec<_> = snapshot
        .backends
        .iter()
        .filter(|backend| {
            matches!(
                backend_account(backend, std::slice::from_ref(account)),
                Some(Some(_))
            )
        })
        .map(|backend| backend.backend_id.clone())
        .collect();
    match backends.as_slice() {
        [one] => Ok(one.clone()),
        [] if account.account_ref.is_some() => anyhow::bail!(
            "account {:?} has no backend; signing in again does not restore it, so create one \
             with `gents config backend set`",
            account.label
        ),
        [] => anyhow::bail!(
            "account {:?} has no backend; create one with `gents config backend set`",
            account.label
        ),
        many => anyhow::bail!(
            "account {:?} runs several backends: {}; name one as backend_id in the profile file",
            account.label,
            many.join(", ")
        ),
    }
}

/// Write `profile`, refusing to create it on, or move it to, an account that
/// cannot serve it. Other edits of a profile on such an account are allowed.
async fn write_checked(
    access: &ConfigAccess,
    snapshot: &Snapshot,
    profile: &InferenceProfile,
    picked_default: bool,
) -> Result<Value> {
    let account = snapshot
        .backends
        .iter()
        .find(|backend| backend.backend_id == profile.backend_id)
        .map(|backend| serving_account(backend, &snapshot.accounts));
    let moved = !snapshot.profiles.iter().any(|stored| {
        stored.profile_id == profile.profile_id && stored.backend_id == profile.backend_id
    });
    if let Some(ServingAccount {
        label,
        state,
        provider: Some(provider),
    }) = &account
    {
        anyhow::ensure!(
            !moved || *state == AccountState::Enabled,
            "account {label:?} is {}; {}",
            state.as_str(),
            gents::oauth_credential::enabled_accounts_note(provider, &snapshot.accounts)
        );
    }
    let doc_id = gents::config_client::write_inference_profile_document(access, profile).await?;
    let mut output = json!({"doc_id":doc_id,"agent_did":profile.agent_did,"profile_id":profile.profile_id,"backend_id":profile.backend_id,"model_name":profile.model_name});
    if let Some(account) = account {
        output["account"] = serde_json::to_value(account)?;
    }
    if picked_default {
        output["picked"] = json!("default");
    }
    Ok(output)
}

async fn target(
    home: Option<&std::path::Path>,
    graphql: Option<&str>,
) -> Result<(crate::CommandAccess, String)> {
    let (access, _) = crate::resolve_config_access(home, graphql).await?;
    let agent_did =
        super::binding::resolve_target_agent_did(None, None, home, graphql, Some(&access)).await?;
    Ok((access, agent_did))
}

pub(super) async fn profile_list(args: ConfigListArgs) -> Result<()> {
    let (access, agent_did) = target(args.home.as_deref(), args.graphql.as_deref()).await?;
    let rows = profile_rows(&access, &agent_did, None).await?;
    match args.output.ensure_supported(
        "config profile list",
        &[OutputFormat::Table, OutputFormat::Json],
    )? {
        OutputFormat::Json => crate::print_json(&json!({
            "collection": gents::Collection::InferenceProfile.graphql_type(),
            "count": rows.len(),
            "items": rows,
        })),
        _ => {
            print!("{}", render_profile_table(&rows));
            Ok(())
        }
    }
}

pub(super) async fn profile_show(args: ConfigShowArgs) -> Result<()> {
    let id = crate::request_helpers::resolve_dual_id(
        "profile",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    args.output
        .ensure_supported("config profile show", &[OutputFormat::Json])?;
    let (access, agent_did) = target(args.home.as_deref(), args.graphql.as_deref()).await?;
    let row = profile_rows(&access, &agent_did, Some(&id))
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("not found: InferenceProfile {agent_did:?}/{id:?}"))?;
    crate::print_json(&row)
}

/// `agent_did`'s profiles (one when `id` is given) as stored, ordered by id,
/// each with the account its backend runs on.
async fn profile_rows(
    access: &ConfigAccess,
    agent_did: &str,
    id: Option<&str>,
) -> Result<Vec<Value>> {
    let mut rows =
        super::crud::query_collection(access, super::crud::PROFILE_SPEC, agent_did, id).await?;
    rows.sort_by(|a, b| a["profile_id"].as_str().cmp(&b["profile_id"].as_str()));
    let snapshot = snapshot(access, agent_did).await?;
    for row in &mut rows {
        if let Some(backend) = snapshot
            .backends
            .iter()
            .find(|backend| row["backend_id"].as_str() == Some(backend.backend_id.as_str()))
        {
            row["account"] = serde_json::to_value(serving_account(backend, &snapshot.accounts))?;
        }
    }
    Ok(rows)
}

/// `config profile list`'s table: the other config lists' columns and the
/// account.
fn render_profile_table(rows: &[Value]) -> String {
    let headers = ["ID", "ENABLED", "NAME", "ACCOUNT"].map(str::to_owned);
    let text = |value: &Value| value.as_str().map(str::trim).unwrap_or_default().to_owned();
    let rendered: Vec<[String; 4]> = rows
        .iter()
        .map(|row| {
            [
                text(&row["profile_id"]),
                row["enabled"]
                    .as_bool()
                    .map(|on| on.to_string())
                    .unwrap_or_default(),
                Some(text(&row["display_name"]))
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| text(&row["name"])),
                match (
                    row["account"]["label"].as_str(),
                    row["account"]["state"].as_str(),
                ) {
                    (Some(label), Some(state)) => format!("{label} ({state})"),
                    _ => String::new(),
                },
            ]
        })
        .collect();
    let mut widths = headers.clone().map(|header| header.len());
    for row in &rendered {
        for (width, value) in widths.iter_mut().zip(row) {
            *width = (*width).max(value.chars().count());
        }
    }
    let line = |cells: &[String; 4]| {
        let line = cells
            .iter()
            .zip(widths)
            .map(|(value, width)| format!("{value:<width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        line.trim_end().to_owned() + "\n"
    };
    std::iter::once(line(&headers))
        .chain(std::iter::once(line(
            &widths.map(|width| "-".repeat(width)),
        )))
        .chain(rendered.iter().map(line))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_model_effort_and_policy_links_are_preserved() {
        let profile=decode_profile(br#"{"agent_did":"owner","profile_id":"chosen","backend_id":"provider","model_name":"exact-model","reasoning_effort":"high","sampling_id":"sampling","execution_id":"execution"}"#).unwrap();
        assert_eq!(profile.model_name, "exact-model");
        assert_eq!(profile.reasoning_effort, Some(gents::ReasoningEffort::High));
        assert_eq!(profile.sampling_id.as_deref(), Some("sampling"));
        assert_eq!(profile.execution_id.as_deref(), Some("execution"));
    }
    #[test]
    fn retired_flat_sampling_and_missing_owner_are_rejected() {
        for input in [
            r#"{"profile_id":"p","backend_id":"b","model_name":"m"}"#,
            r#"{"agent_did":"owner","profile_id":"p","backend_id":"b","model_name":"m","temperature":1}"#,
        ] {
            assert!(decode_profile(input.as_bytes()).is_err());
        }
    }

    const DID: &str = "did:key:z6MkTestProfileAccounts";
    const CLAUDE: &str = "claude-subscription";

    fn claude_sign_in(who: &str) -> gents::oauth_credential::OAuthCredential {
        gents::claude_oauth::credential_from_login_tokens(
            DID,
            CLAUDE,
            &gents::claude_oauth::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: format!("refresh-SECRET-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("IDENTITY-{who}")),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        )
    }

    fn backend(backend_id: &str, name: &str, provider_kind: &str, auth: Value) -> Value {
        let endpoint = if provider_kind == "ClaudeCliSubscription" {
            "claude-cli://subscription"
        } else {
            "http://127.0.0.1:1/v1"
        };
        json!({"agent_did": DID, "backend_id": backend_id, "name": name,
            "provider_kind": provider_kind, "endpoint": endpoint, "auth": auth})
    }

    async fn write_profile(access: &ConfigAccess, profile_id: &str, backend_id: &str) {
        let profile = serde_json::from_value(json!({
            "agent_did": DID, "profile_id": profile_id, "backend_id": backend_id,
            "model_name": "model-x",
        }))
        .unwrap();
        gents::config_client::write_inference_profile_document(access, &profile)
            .await
            .unwrap();
    }

    /// Claude: the original account as a raw pre-#2116 row (no label, no
    /// reference), `label-b` added, `label-c` added then disabled; a backend
    /// whose account is not on this node and an API-key backend; one profile
    /// on each. Returns the access and B's and C's backend ids.
    async fn seeded() -> (ConfigAccess, String, String) {
        use gents::oauth_credential::{set_account_enabled, store_sign_in};
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node);
        for value in [
            backend(
                "claude",
                "Claude",
                "ClaudeCliSubscription",
                json!({"kind": "principal_oauth"}),
            ),
            backend(
                "gone",
                "Gone",
                "ClaudeCliSubscription",
                json!({"kind": "principal_oauth", "account_ref": "acct-other"}),
            ),
            backend(
                "openai",
                "OpenAI",
                "OpenAiCompatible",
                json!({"kind": "api_key", "key": "key-SECRET"}),
            ),
        ] {
            gents::config_client::write_inference_backend_document(
                &access,
                &serde_json::from_value(value).unwrap(),
            )
            .await
            .unwrap();
        }
        gents::oauth_credential::upsert_oauth_credential_on(&access, &claude_sign_in("a"))
            .await
            .unwrap();
        let b = store_sign_in(&access, claude_sign_in("b"), Some("label-b"))
            .await
            .unwrap();
        let c = store_sign_in(&access, claude_sign_in("c"), Some("label-c"))
            .await
            .unwrap();
        set_account_enabled(&access, DID, &c.credential.credential_id, false)
            .await
            .unwrap();
        let backend_of = |sign_in: &gents::oauth_credential::SignIn| {
            format!(
                "{CLAUDE}-{}",
                sign_in.credential.account_ref.as_deref().unwrap()
            )
        };
        let (b_backend, c_backend) = (backend_of(&b), backend_of(&c));
        for (profile_id, backend_id) in [
            ("p-original", "claude"),
            ("p-b", b_backend.as_str()),
            ("p-c", c_backend.as_str()),
            ("p-gone", "gone"),
            ("p-api", "openai"),
        ] {
            write_profile(&access, profile_id, backend_id).await;
        }
        (access, b_backend, c_backend)
    }

    fn assert_redacted(text: &str) {
        assert!(
            !text.contains("SECRET") && !text.contains("IDENTITY"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn list_and_show_name_each_profiles_account() {
        let (access, _, _) = seeded().await;
        let rows = profile_rows(&access, DID, None).await.unwrap();
        let accounts: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row["profile_id"].as_str().unwrap(),
                    row["account"]["label"].as_str().unwrap_or("-"),
                    row["account"]["state"].as_str().unwrap_or("-"),
                )
            })
            .collect();
        assert_eq!(
            accounts,
            [
                ("p-api", "OpenAI", "enabled"),
                ("p-b", "label-b", "enabled"),
                ("p-c", "label-c", "disabled"),
                ("p-gone", "Gone", "account not on this node"),
                ("p-original", "Claude", "enabled"),
            ]
        );
        assert_redacted(&serde_json::to_string(&rows).unwrap());
        let shown = profile_rows(&access, DID, Some("p-b")).await.unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(
            shown[0]["account"],
            json!({"label": "label-b", "state": "enabled"})
        );
    }

    #[tokio::test]
    async fn the_missing_state_reads_the_same_in_accounts_and_profiles() {
        let (access, _, _) = seeded().await;
        let accounts =
            crate::commands::accounts::account_rows(&access, DID, None, chrono::Utc::now())
                .await
                .unwrap();
        let gone = accounts
            .iter()
            .find(|row| row.backend_id.as_deref() == Some("gone"))
            .expect("the missing-account backend row");
        let profile = profile_rows(&access, DID, Some("p-gone")).await.unwrap();
        assert_eq!(profile[0]["account"]["state"], json!(gone.status));
    }

    #[tokio::test]
    async fn the_table_has_an_account_column() {
        let (access, _, _) = seeded().await;
        let table = render_profile_table(&profile_rows(&access, DID, None).await.unwrap());
        let mut lines = table.lines();
        assert!(lines.next().unwrap().ends_with("ACCOUNT"), "{table}");
        assert!(table.contains("label-c (disabled)"), "{table}");
        assert!(table.contains("Gone (account not on this node)"), "{table}");
        assert_redacted(&table);
    }

    async fn stored_profiles(access: &ConfigAccess) -> Vec<InferenceProfile> {
        crate::commands::accounts::snapshot(access, DID)
            .await
            .unwrap()
            .profiles
    }

    async fn stored(access: &ConfigAccess, profile_id: &str) -> Option<InferenceProfile> {
        stored_profiles(access)
            .await
            .into_iter()
            .find(|profile| profile.profile_id == profile_id)
    }

    fn file(profile_id: &str, backend_id: Option<&str>) -> Vec<u8> {
        let mut value =
            json!({"agent_did": DID, "profile_id": profile_id, "model_name": "model-x"});
        if let Some(backend_id) = backend_id {
            value["backend_id"] = json!(backend_id);
        }
        serde_json::to_vec(&value).unwrap()
    }

    async fn disable(access: &ConfigAccess, credential_id: &str) {
        gents::oauth_credential::set_account_enabled(access, DID, credential_id, false)
            .await
            .unwrap();
    }

    fn original_id() -> String {
        gents::oauth_credential::oauth_credential_id(DID, CLAUDE)
    }

    #[tokio::test]
    async fn set_with_an_account_uses_its_backend() {
        let (access, b_backend, _) = seeded().await;
        let mut grok = claude_sign_in("g");
        grok.provider = "xai-oauth".into();
        grok.credential_id = format!("xai-oauth:{DID}:acct-g");
        grok.account_ref = Some("acct-g".into());
        grok.label = Some("label-b".into());
        gents::oauth_credential::upsert_oauth_credential_on(&access, &grok)
            .await
            .unwrap();

        let output = set_profile(&access, &file("new-p", None), Some("label-b"), Some(CLAUDE))
            .await
            .unwrap();
        assert_eq!(output["backend_id"], json!(b_backend));
        assert_eq!(
            output["account"],
            json!({"label": "label-b", "state": "enabled"})
        );
        assert_eq!(
            stored(&access, "new-p").await.unwrap().backend_id,
            b_backend
        );

        let error = set_profile(&access, &file("new-q", None), Some("label-b"), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("pass --provider"), "{error}");
        assert!(stored(&access, "new-q").await.is_none());
    }

    #[tokio::test]
    async fn set_with_a_provider_picks_the_default_account_and_says_so() {
        let (access, b_backend, _) = seeded().await;
        let output = set_profile(&access, &file("new-p", None), None, Some(CLAUDE))
            .await
            .unwrap();
        assert_eq!(output["backend_id"], json!("claude"));
        assert_eq!(output["picked"], json!("default"));
        disable(&access, &original_id()).await;
        let output = set_profile(&access, &file("new-q", None), None, Some(CLAUDE))
            .await
            .unwrap();
        assert_eq!(
            output["backend_id"],
            json!(b_backend),
            "never the disabled first account"
        );
        assert_eq!(
            output["account"],
            json!({"label": "label-b", "state": "enabled"})
        );
    }

    #[tokio::test]
    async fn creating_on_an_unavailable_account_is_refused_with_the_enabled_list() {
        let (access, b_backend, _) = seeded().await;
        let error = set_profile(&access, &file("new-p", None), Some("label-c"), Some(CLAUDE))
            .await
            .unwrap_err()
            .to_string();
        for needle in ["\"label-c\"", "disabled", "\"Claude\"", "\"label-b\""] {
            assert!(error.contains(needle), "{needle}: {error}");
        }
        let error = set_profile(&access, &file("new-p", Some("gone")), None, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("\"Gone\" is account not on this node"),
            "{error}"
        );
        assert!(stored(&access, "new-p").await.is_none());

        disable(&access, &original_id()).await;
        let b = stored_profiles(&access).await;
        let b_id = crate::commands::accounts::snapshot(&access, DID)
            .await
            .unwrap()
            .accounts
            .into_iter()
            .find(|account| account.label == "label-b")
            .unwrap()
            .credential_id;
        disable(&access, &b_id).await;
        let error = set_profile(&access, &file("new-p", Some(&b_backend)), None, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("claude-login"), "{error}");
        assert_eq!(stored_profiles(&access).await, b, "nothing written");
    }

    #[tokio::test]
    async fn other_edits_on_a_disabled_account_are_allowed() {
        let (access, _, c_backend) = seeded().await;
        let mut value: Value = serde_json::from_slice(&file("p-c", Some(&c_backend))).unwrap();
        value["max_output_tokens"] = json!(99);
        set_profile(&access, &serde_json::to_vec(&value).unwrap(), None, None)
            .await
            .unwrap();
        assert_eq!(
            stored(&access, "p-c").await.unwrap().max_output_tokens,
            Some(99)
        );
    }

    #[tokio::test]
    async fn a_file_naming_another_backend_conflicts_with_account() {
        let (access, _, _) = seeded().await;
        let error = set_profile(
            &access,
            &file("new-p", Some("openai")),
            Some("label-b"),
            Some(CLAUDE),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("openai"), "{error}");
        assert!(stored(&access, "new-p").await.is_none());
    }

    #[tokio::test]
    async fn an_account_with_several_backends_is_ambiguous() {
        let (access, _, _) = seeded().await;
        gents::config_client::write_inference_backend_document(
            &access,
            &serde_json::from_value(backend(
                "claude-2",
                "Claude 2",
                "ClaudeCliSubscription",
                json!({"kind": "principal_oauth"}),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
        let error = set_profile(&access, &file("new-p", None), Some("Claude"), Some(CLAUDE))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("claude") && error.contains("claude-2"),
            "{error}"
        );
        assert!(stored(&access, "new-p").await.is_none());
    }

    #[tokio::test]
    async fn set_account_moves_only_the_backend() {
        let (access, b_backend, _) = seeded().await;
        let before = stored(&access, "p-original").await.unwrap();
        let output = set_account(&access, DID, "p-original", "label-b", Some(CLAUDE))
            .await
            .unwrap();
        assert_eq!(
            output["account"],
            json!({"label": "label-b", "state": "enabled"})
        );
        let after = stored(&access, "p-original").await.unwrap();
        assert_eq!(
            after,
            InferenceProfile {
                backend_id: b_backend,
                ..before.clone()
            }
        );
        let error = set_account(&access, DID, "p-original", "label-c", Some(CLAUDE))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("disabled"), "{error}");
        let error = set_account(&access, DID, "p-original", "label-unknown", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no account"), "{error}");
        assert_eq!(stored(&access, "p-original").await.unwrap(), after);
    }
}
