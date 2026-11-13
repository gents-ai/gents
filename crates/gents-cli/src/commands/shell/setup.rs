use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use gents_desktop_core::client::apply_config_components_on;
use gents_desktop_core::inference::{discover_models, load_inference_setup_state};
use gents_documents::{AdvertisedModel, BackendAuth, PackConfig};
use gents_inference::auth::credential::effective_account_label;
use gents_inference::backends::setup::{
    connection_spec, inference_setup_catalog, model_option, InferenceAuthMethod,
    InferenceConnectionSpec, InferenceModelOption, InferenceModelRecommendation,
    InferenceProviderId, InferenceProviderOption, RecommendedIntegerControl,
    RecommendedNumberControl,
};
use gents_shell::source::{
    Discovered, SavedAccount, SettingKey, SetupAuth, SetupConnection, SetupMethod, SetupModel,
    SetupProvider, SetupSetting,
};
use gents_store::config_client::ConfigAccess;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::cli::args::BackendPresetArg;
use crate::commands::accounts::AccountRow;
use crate::commands::claude_login::{run_claude_login, ClaudeLoginOptions};
use crate::commands::codex_login::{run_codex_login, CodexLoginOptions};
use crate::commands::grok_login::{run_grok_login, GrokLoginOptions};

pub(super) const ALREADY_WAITING: &str = "A sign-in is already waiting. Finish or cancel it first.";
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// The desktop's add-backend choices, each sign-in with its provider's enabled accounts.
pub(super) fn catalog(accounts: &[AccountRow]) -> Vec<SetupProvider> {
    let providers = inference_setup_catalog().providers.into_iter();
    let shown = |provider: InferenceProviderOption| {
        let methods = provider.auth_options.into_iter().map(|option| SetupMethod {
            id: name(option.method),
            name: option.display_name,
            endpoint: option.default_endpoint,
            endpoint_fixed: provider.id == InferenceProviderId::Anthropic
                && option.method == InferenceAuthMethod::ApiKey,
            auth: auth(option.method),
            accounts: saved(accounts, provider.id, option.method),
        });
        let methods = methods.collect();
        SetupProvider {
            id: name(provider.id),
            name: provider.display_name,
            about: provider.description,
            methods,
        }
    };
    providers.map(shown).collect()
}

fn auth(method: InferenceAuthMethod) -> SetupAuth {
    use InferenceAuthMethod::*;
    match method {
        ChatGptOauth | ClaudeOauth | GrokOauth => SetupAuth::SignIn,
        ApiKey => SetupAuth::Key,
        OptionalApiKey => SetupAuth::OptionalKey,
    }
}

/// A provider's first account has no `account_ref`: here it is the empty one.
fn saved(
    accounts: &[AccountRow],
    provider: InferenceProviderId,
    method: InferenceAuthMethod,
) -> Vec<SavedAccount> {
    let spec = connection_spec(provider, method, "").ok();
    let oauth = spec.and_then(|spec| spec.oauth_provider);
    let matching = accounts
        .iter()
        .filter(|row| row.status == "enabled" && Some(row.provider.as_str()) == oauth);
    matching
        .map(|row| SavedAccount {
            label: row.label.clone(),
            account_ref: row.account_ref.clone().unwrap_or_default(),
        })
        .collect()
}

/// Discovery is the connection test, as the desktop's: a sign-in reads its stored account, and
/// the models found are recorded on the backends of that connection.
pub(super) async fn discover(
    access: &ConfigAccess,
    node_did: &str,
    connection: &SetupConnection,
) -> Result<Discovered, String> {
    discovered(access, node_did, connection)
        .await
        .map_err(|error| redact(format!("{error:#}"), connection))
}

async fn discovered(
    access: &ConfigAccess,
    node_did: &str,
    connection: &SetupConnection,
) -> anyhow::Result<Discovered> {
    let (provider, method) = choice(&connection.provider, &connection.method)?;
    let found = discover_models(
        Some(access),
        node_did,
        provider,
        method,
        &connection.endpoint,
        key(connection),
        account(connection),
    )
    .await?;
    Ok(Discovered {
        models: found.models.into_iter().map(setup_model).collect(),
        failure: found.failure.map(|(_, text)| redact(text, connection)),
        manual: found.manual_entry_allowed,
    })
}

/// The recommended settings for a model id the provider did not advertise.
pub(super) fn recommend(connection: &SetupConnection, model: &str) -> Result<SetupModel, String> {
    let advertised = AdvertisedModel {
        model_name: model.trim().to_string(),
        display_name: None,
        context_window: None,
        max_context_window: None,
        max_output_tokens: None,
        reasoning_efforts: None,
    };
    choice(&connection.provider, &connection.method)
        .and_then(|(provider, method)| model_option(provider, method, advertised))
        .map(setup_model)
        .map_err(|error| format!("{error:#}"))
}

fn setup_model(option: InferenceModelOption) -> SetupModel {
    let InferenceModelOption {
        advertised,
        recommendation,
    } = option;
    SetupModel {
        name: advertised
            .display_name
            .unwrap_or_else(|| advertised.model_name.clone()),
        id: advertised.model_name,
        settings: settings(&recommendation),
    }
}

/// Each control the recommendation has, in the desktop's order. Concurrency is always there.
fn settings(recommended: &InferenceModelRecommendation) -> Vec<SetupSetting> {
    let reasoning = recommended
        .reasoning_effort
        .as_ref()
        .map(|control| SetupSetting {
            key: SettingKey::ReasoningEffort,
            value: name(control.recommended),
            min: None,
            max: None,
            choices: control.choices.iter().copied().map(name).collect(),
        });
    let whole = |key, control: &RecommendedIntegerControl| {
        number(key, control.recommended, control.min, control.max)
    };
    let decimal = |key, control: &RecommendedNumberControl| {
        number(key, control.recommended, control.min, control.max)
    };
    [
        recommended
            .context_window
            .as_ref()
            .map(|control| whole(SettingKey::ContextWindow, control)),
        recommended
            .max_output_tokens
            .as_ref()
            .map(|control| whole(SettingKey::MaxOutputTokens, control)),
        recommended
            .temperature
            .as_ref()
            .map(|control| decimal(SettingKey::Temperature, control)),
        recommended
            .top_p
            .as_ref()
            .map(|control| decimal(SettingKey::TopP, control)),
        reasoning,
        Some(whole(
            SettingKey::MaxConcurrent,
            &recommended.max_concurrent,
        )),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn number<T>(key: SettingKey, recommended: T, min: T, max: Option<T>) -> SetupSetting
where
    T: ToString,
{
    SetupSetting {
        key,
        value: recommended.to_string(),
        min: Some(min.to_string()),
        max: max.map(|max| max.to_string()),
        choices: Vec::new(),
    }
}

/// `provider`, or the first of `{provider}-2`, `-3`… that `existing` does not hold.
pub(super) fn backend_id(provider: &str, existing: &[String]) -> String {
    let mut id = provider.to_string();
    let mut suffix = 2;
    while existing.contains(&id) {
        id = format!("{provider}-{suffix}");
        suffix += 1;
    }
    id
}

/// What `save` wrote: its notice, and the profile an agent runs the model on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Saved {
    pub(crate) notice: String,
    pub(crate) profile: String,
}

fn profile_id(backend_id: &str) -> String {
    format!("profile-{backend_id}")
}

/// One backend beside the node's others and its profile, as the desktop's add-backend writes
/// them: it binds no agent.
pub(super) async fn save(
    access: &ConfigAccess,
    node_did: &str,
    connection: &SetupConnection,
    model: &str,
    settings: &[(SettingKey, String)],
) -> Result<Saved, String> {
    let model = model.trim();
    let saved = async {
        let (provider, method) = choice(&connection.provider, &connection.method)?;
        let spec = connection_spec(provider, method, &connection.endpoint)?;
        let state = load_inference_setup_state(access, node_did).await?;
        let existing: Vec<_> = state
            .inference_backends
            .into_iter()
            .map(|backend| backend.backend_id)
            .collect();
        let id = backend_id(&connection.provider, &existing);
        let auth = backend_auth(&spec, connection);
        let document = backend_document(node_did, &id, &spec, auth, model, settings)?;
        apply_config_components_on(access, &document).await?;
        anyhow::Ok(id)
    };
    let saved = saved.await;
    let profile = saved.as_deref().map(profile_id).unwrap_or_default();
    let notice = saved_answer(saved, model, connection)?;
    Ok(Saved { notice, profile })
}

/// A sign-in on a node with no backend of its provider's kind gets the backend and profile
/// `save` writes for the provider's default model with its recommended settings. Any backend of
/// that kind leaves the node as it is: further accounts join it through a pool or a profile.
pub(crate) async fn connect_first_sign_in(
    access: &ConfigAccess,
    node_did: &str,
    credential: &gents_inference::auth::credential::OAuthCredential,
) -> anyhow::Result<Option<Saved>> {
    let options = inference_setup_catalog().providers.into_iter();
    let options = options.flat_map(|provider| {
        let methods = provider.auth_options.into_iter();
        methods.map(move |option| (provider.id, option.method))
    });
    let signed_in = options
        .filter_map(|(provider, method)| {
            let spec = connection_spec(provider, method, "").ok()?;
            (spec.oauth_provider == Some(credential.provider.as_str()))
                .then_some((provider, method, spec))
        })
        .next();
    let Some((provider, method, spec)) = signed_in else {
        return Ok(None);
    };
    let preset = match method {
        InferenceAuthMethod::ChatGptOauth => BackendPresetArg::ChatGptCodex,
        InferenceAuthMethod::ClaudeOauth => BackendPresetArg::ClaudeCliSubscription,
        InferenceAuthMethod::GrokOauth => BackendPresetArg::XaiGrokOAuth,
        InferenceAuthMethod::ApiKey | InferenceAuthMethod::OptionalApiKey => return Ok(None),
    };
    let Some(model) = preset.default_model_name() else {
        return Ok(None);
    };
    let state = load_inference_setup_state(access, node_did).await?;
    let kind = spec.provider_kind.as_str();
    let served = |backend: &gents_desktop_core::inference::InferenceBackendView| {
        backend.provider_kind.as_deref() == Some(kind)
    };
    if state.inference_backends.iter().any(served) {
        return Ok(None);
    }
    let connection = SetupConnection {
        provider: name(provider),
        method: name(method),
        endpoint: String::new(),
        key: None,
        account: credential.account_ref.clone(),
    };
    let model = recommend(&connection, model).map_err(anyhow::Error::msg)?;
    let settings = model.settings.into_iter();
    let settings: Vec<_> = settings.map(|setting| (setting.key, setting.value)).collect();
    let saved = save(access, node_did, &connection, &model.id, &settings).await;
    saved.map(Some).map_err(anyhow::Error::msg)
}

/// A save's notice, or why it failed, which never carries the key.
fn saved_answer(
    saved: anyhow::Result<String>,
    model: &str,
    connection: &SetupConnection,
) -> Result<String, String> {
    match saved {
        Ok(id) => Ok(format!("saved {id}: {model}")),
        Err(error) => Err(redact(format!("could not save: {error:#}"), connection)),
    }
}

fn backend_document(
    node_did: &str,
    backend_id: &str,
    spec: &InferenceConnectionSpec,
    auth: BackendAuth,
    model: &str,
    settings: &[(SettingKey, String)],
) -> anyhow::Result<PackConfig> {
    let value = |key| setting(settings, key);
    let profile_id = profile_id(backend_id);
    let (temperature, top_p) = (value(SettingKey::Temperature)?, value(SettingKey::TopP)?);
    let sampled = !temperature.is_null() || !top_p.is_null();
    let sampling_id = sampled.then(|| format!("{profile_id}-sampling"));
    let sampling: Vec<_> = sampling_id
        .iter()
        .map(|sampling_id| {
            json!({
                "node_did": node_did,
                "sampling_id": sampling_id,
                "display_name": format!("{} recommended sampling", spec.backend_name),
                "temperature": temperature,
                "top_p": top_p,
            })
        })
        .collect();
    let document = json!({
        "node": { "node_did": node_did },
        "inference_backends": [{
            "node_did": node_did,
            "backend_id": backend_id,
            "name": spec.backend_name,
            "provider_kind": spec.provider_kind,
            "openai_wire_api": spec.openai_wire_api,
            "endpoint": spec.endpoint,
            "auth": auth,
            "max_concurrent": value(SettingKey::MaxConcurrent)?,
            "max_queue_depth": 100,
            "enabled": true,
        }],
        "inference_profiles": [{
            "node_did": node_did,
            "profile_id": profile_id,
            "display_name": spec.backend_name,
            "backend_id": backend_id,
            "model_name": model,
            "reasoning_effort": value(SettingKey::ReasoningEffort)?,
            "context_window": value(SettingKey::ContextWindow)?,
            "max_output_tokens": value(SettingKey::MaxOutputTokens)?,
            "sampling_id": sampling_id,
        }],
        "inference_sampling": sampling,
    });
    Ok(serde_json::from_value(document)?)
}

/// The shell sends only the settings the recommendation has, already checked: an absent one
/// is null.
fn setting(settings: &[(SettingKey, String)], key: SettingKey) -> anyhow::Result<Value> {
    let Some((_, text)) = settings.iter().find(|(shown, _)| *shown == key) else {
        return Ok(Value::Null);
    };
    let text = text.trim();
    let label = key.label();
    Ok(match key {
        SettingKey::ReasoningEffort => Value::from(text),
        // As the shell checks it: "4.0" is whole.
        key if key.whole() => {
            let whole = text.parse::<f64>().ok().filter(|number| {
                number.fract() == 0.0 && (i64::MIN as f64..i64::MAX as f64).contains(number)
            });
            Value::from(whole.with_context(|| format!("{label} must be a whole number"))? as i64)
        }
        _ => Value::from(
            text.parse::<f64>()
                .with_context(|| format!("{label} must be a number"))?,
        ),
    })
}

/// Signs in through this crate's own sign-ins, handing `on_url` the page to open. The run stops
/// when `cancel` fires or the browser takes too long, and keeps tokens issued while it stops.
pub(super) async fn sign_in<U>(
    access: &ConfigAccess,
    node_did: &str,
    provider: &str,
    method: &str,
    label: Option<String>,
    on_url: U,
    cancel: &CancellationToken,
) -> Result<SavedAccount, String>
where
    U: FnOnce(String),
{
    let text = |error: anyhow::Error| format!("{error:#}");
    let (provider, method) = choice(provider, method).map_err(text)?;
    let spec = connection_spec(provider, method, "").map_err(text)?;
    let provider = spec.oauth_provider.unwrap_or_default().to_string();
    let signed = match method {
        InferenceAuthMethod::ChatGptOauth => {
            let options = CodexLoginOptions {
                provider,
                label,
                client_id: None,
                issuer: None,
                device_auth: false,
            };
            let login =
                run_codex_login(access, node_did, &options, |url| on_url(url.into()), cancel);
            waited(login, cancel, "ChatGPT")
                .await
                .map(|done| done.sign_in)
        }
        InferenceAuthMethod::ClaudeOauth => {
            let options = ClaudeLoginOptions {
                provider,
                label,
                manual: false,
                open_browser: true,
                client_id: None,
                token_url: None,
            };
            let login =
                run_claude_login(access, node_did, &options, |url| on_url(url.into()), cancel);
            waited(login, cancel, "Claude")
                .await
                .map(|done| done.sign_in)
        }
        InferenceAuthMethod::GrokOauth => {
            let options = GrokLoginOptions { provider, label };
            // The Claude and ChatGPT sign-ins open their own pages.
            let shown = |url: &str, code: Option<&str>| {
                if let Err(error) = webbrowser::open(url) {
                    tracing::warn!(%error, "could not open the Grok sign-in URL in a browser");
                }
                match code {
                    Some(code) => on_url(format!("{url}   code {code}")),
                    None => on_url(url.into()),
                }
            };
            let login = run_grok_login(access, node_did, &options, shown, cancel);
            waited(login, cancel, "Grok").await.map(|done| done.sign_in)
        }
        InferenceAuthMethod::ApiKey | InferenceAuthMethod::OptionalApiKey => {
            return Err("an API key needs no sign-in".to_string());
        }
    }?;
    let credential = signed.credential;
    Ok(SavedAccount {
        label: effective_account_label(&credential),
        account_ref: credential.account_ref.unwrap_or_default(),
    })
}

/// Past the wait, `login` is stopped through `cancel`, never dropped: an exchange the provider
/// already accepted drains, and its tokens are kept.
pub(super) async fn waited<F, T>(
    login: F,
    cancel: &CancellationToken,
    product: &str,
) -> Result<T, String>
where
    F: Future<Output = anyhow::Result<T>>,
{
    tokio::pin!(login);
    tokio::select! {
        signed = &mut login => return signed.map_err(|error| format!("{error:#}")),
        () = tokio::time::sleep(SIGN_IN_WAIT) => {}
    }
    cancel.cancel();
    login
        .await
        .map_err(|_| format!("{product} sign-in timed out waiting for the browser."))
}

fn choice(
    provider: &str,
    method: &str,
) -> anyhow::Result<(InferenceProviderId, InferenceAuthMethod)> {
    Ok((named(provider)?, named(method)?))
}

fn named<T>(id: &str) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(Value::from(id)).with_context(|| format!("unknown setup choice {id:?}"))
}

fn name<T>(value: T) -> String
where
    T: Serialize,
{
    let value = serde_json::to_value(value).ok();
    let name = value.as_ref().and_then(Value::as_str);
    name.unwrap_or_default().to_string()
}

fn key(connection: &SetupConnection) -> Option<&str> {
    let key = connection.key.as_ref().map(|key| key.0.trim());
    key.filter(|key| !key.is_empty())
}

fn account(connection: &SetupConnection) -> Option<&str> {
    let account = connection.account.as_deref();
    account.filter(|account| !account.is_empty())
}

fn backend_auth(spec: &InferenceConnectionSpec, connection: &SetupConnection) -> BackendAuth {
    match (spec.oauth_provider, key(connection)) {
        (Some(_), _) => BackendAuth::NodeOAuth {
            account_ref: account(connection).map(str::to_owned),
        },
        (None, Some(key)) => BackendAuth::ApiKey {
            key: key.to_string(),
        },
        (None, None) => BackendAuth::Unauthenticated,
    }
}

/// A provider may echo the key it refused, so no text leaving here carries it.
fn redact(text: String, connection: &SetupConnection) -> String {
    match key(connection) {
        Some(key) => text.replace(key, "…"),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use gents_shell::source::Secret;
    use gents_store::collection::Collection;
    use gents_store::config_client::GraphqlEndpoint;

    use super::*;

    #[test]
    fn backend_ids_skip_the_taken_ones() {
        let taken = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();

        assert_eq!(backend_id("anthropic", &[]), "anthropic");
        assert_eq!(
            backend_id("anthropic", &taken(&["anthropic"])),
            "anthropic-2"
        );
        assert_eq!(
            backend_id("anthropic", &taken(&["anthropic", "anthropic-2"])),
            "anthropic-3"
        );
    }

    fn claude_account(label: &str, status: &str, account_ref: &str) -> AccountRow {
        AccountRow {
            provider: "claude-subscription".to_string(),
            label: label.to_string(),
            identity: None,
            plan: None,
            status: status.to_string(),
            default: false,
            credential_id: Some(format!("credential-{account_ref}")),
            account_ref: Some(account_ref.to_string()),
            backend_id: None,
            profiles: Vec::new(),
            usage: None,
            read: None,
        }
    }

    fn method<'a>(providers: &'a [SetupProvider], provider: &str, id: &str) -> &'a SetupMethod {
        let provider = providers.iter().find(|shown| shown.id == provider).unwrap();
        provider
            .methods
            .iter()
            .find(|shown| shown.id == id)
            .unwrap()
    }

    #[test]
    fn the_catalog_mirrors_the_desktop_choices() {
        let providers = catalog(&[
            claude_account("Claude 2", "enabled", "ref-2"),
            claude_account("Claude 3", "disabled", "ref-3"),
        ]);

        let anthropic = providers.iter().find(|shown| shown.id == "anthropic");
        let anthropic = &anthropic.unwrap().methods;
        let shown: Vec<_> = anthropic
            .iter()
            .map(|method| (method.id.as_str(), method.auth, method.endpoint_fixed))
            .collect();
        assert_eq!(
            shown,
            [
                ("claude_oauth", SetupAuth::SignIn, false),
                ("api_key", SetupAuth::Key, true),
            ]
        );
        let local = method(&providers, "local", "optional_api_key");
        assert_eq!(local.auth, SetupAuth::OptionalKey);
        assert_eq!(local.endpoint, "http://127.0.0.1:11434/v1");
        let claude = method(&providers, "anthropic", "claude_oauth");
        let saved = SavedAccount {
            label: "Claude 2".to_string(),
            account_ref: "ref-2".to_string(),
        };
        assert_eq!(claude.accounts, [saved]);
    }

    fn owner_with_anthropic() -> Value {
        json!({
            "node": { "node_did": "owner" },
            "default_agent_selection": { "node_did": "owner", "default_agent_did": "engineer" },
            "inference_backends": [{
                "node_did": "owner",
                "backend_id": "anthropic",
                "name": "Anthropic",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:1/v1",
                "auth": { "kind": "unauthenticated" },
            }],
            "inference_profiles": [{
                "node_did": "owner",
                "profile_id": "profile-anthropic",
                "backend_id": "anthropic",
                "model_name": "claude-old",
            }],
            "agents": [
                { "node_did": "owner", "agent_did": "engineer", "inference_profile_id": "profile-anthropic" },
            ],
        })
    }

    fn api_key(provider: &str, endpoint: &str, key: &str) -> SetupConnection {
        SetupConnection {
            provider: provider.to_string(),
            method: "api_key".to_string(),
            endpoint: endpoint.to_string(),
            key: Some(Secret(key.to_string())),
            account: None,
        }
    }

    /// The node `served_config` publishes a config's `owner` as.
    const OWNER: &str = gents_session::test_support::lineage_fixtures::FIXTURE_DID;

    /// The owner's `collection` documents, whole, as the store reads them back.
    async fn stored(access: &ConfigAccess, collection: Collection) -> Vec<Value> {
        let (name, key) = (collection.graphql_type(), collection.unique_field());
        let query =
            format!(r#"{{ {name}(filter: {{ node_did: {{ _eq: "{OWNER}" }} }}) {{ {key} }} }}"#);
        let response = access.execute(&query).await.unwrap();
        let rows = gents_protocol::graphql::graphql_rows_from_response(&response, name);
        let ids: Vec<String> = rows
            .iter()
            .map(|row| row[key].as_str().unwrap().to_owned())
            .collect();
        let ids = &ids;
        access
            .transact_readonly("test.setup.stored", |txn| {
                Box::pin(async move {
                    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                    let records = gents_documents::read_desired_state_records_in_txn(
                        txn, collection, OWNER, &ids,
                    )
                    .await?;
                    anyhow::Ok(
                        records
                            .into_values()
                            .map(|(_, value)| value)
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .await
            .unwrap()
    }

    fn by(documents: &[Value], field: &str, id: &str) -> Value {
        let found = documents.iter().find(|document| document[field] == id);
        found.cloned().unwrap_or_default()
    }

    #[tokio::test]
    async fn saving_adds_a_backend_beside_the_existing_one() {
        let served = gents_host::test_support::served_config(owner_with_anthropic()).await;
        let access = ConfigAccess::Graphql(served.user.clone());
        let backends = stored(&access, Collection::InferenceBackend).await;
        let profiles = stored(&access, Collection::InferenceProfile).await;
        let agents = stored(&access, Collection::Agent).await;
        let connection = api_key("anthropic", "", "sk-new");
        let settings = [
            (SettingKey::MaxConcurrent, "2"),
            (SettingKey::Temperature, "0.7"),
            (SettingKey::TopP, "0.95"),
        ]
        .map(|(key, value)| (key, value.to_string()));

        let saved = save(&access, OWNER, &connection, "claude-x", &settings).await;

        let expected = Saved {
            notice: "saved anthropic-2: claude-x".to_string(),
            profile: "profile-anthropic-2".to_string(),
        };
        assert_eq!(saved, Ok(expected));
        let backends_now = stored(&access, Collection::InferenceBackend).await;
        let profiles_now = stored(&access, Collection::InferenceProfile).await;
        let existing = by(&backends, "backend_id", "anthropic");
        assert_ne!(existing, Value::Null);
        assert_eq!(by(&backends_now, "backend_id", "anthropic"), existing);
        let existing = by(&profiles, "profile_id", "profile-anthropic");
        assert_ne!(existing, Value::Null);
        assert_eq!(
            by(&profiles_now, "profile_id", "profile-anthropic"),
            existing
        );
        let added = by(&backends_now, "backend_id", "anthropic-2");
        assert_eq!(added["auth"]["kind"], json!("credential"));
        let credential =
            gents_store::graphql::escape_graphql_string(added["auth"]["id"].as_str().unwrap());
        let runtime =
            ConfigAccess::local_as(served.node.clone(), ::identity::Did::new(OWNER).unwrap());
        let secret = runtime
            .execute(&format!(
                r#"{{ BackendCredential(filter: {{ credential_id: {{ _eq: "{credential}" }} }}) {{ secret }} }}"#
            ))
            .await
            .unwrap();
        let secret = &secret["data"]["BackendCredential"][0]["secret"];
        assert_eq!(*secret, json!({ "kind": "api_key", "key": "sk-new" }));
        let limits = [&added["max_concurrent"], &added["max_queue_depth"]];
        assert_eq!(limits, [&json!(2), &json!(100)]);
        assert_eq!(added["enabled"], json!(true));
        let profile = by(&profiles_now, "profile_id", "profile-anthropic-2");
        let bound = [&profile["model_name"], &profile["backend_id"]];
        assert_eq!(bound, [&json!("claude-x"), &json!("anthropic-2")]);
        let sampled = "profile-anthropic-2-sampling";
        assert_eq!(profile["sampling_id"], json!(sampled));
        let [sampling] = &stored(&access, Collection::InferenceSampling).await[..] else {
            panic!("one sampling");
        };
        assert_eq!(sampling["sampling_id"], json!(sampled));
        let named = [
            &added["name"],
            &profile["display_name"],
            &sampling["display_name"],
        ];
        assert_eq!(
            named,
            [
                &json!("Anthropic API"),
                &json!("Anthropic API"),
                &json!("Anthropic API recommended sampling")
            ]
        );
        let values = [&sampling["temperature"], &sampling["top_p"]];
        assert_eq!(values, [&json!(0.7), &json!(0.95)]);
        assert_eq!(stored(&access, Collection::Agent).await, agents);

        let signed_in = gents_inference::auth::claude::credential_from_login_tokens(
            OWNER,
            gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &gents_inference::auth::claude::ClaudeLoginTokens {
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                expires_in: Some(3600),
                scope: None,
                account_id: None,
                organization_uuid: None,
                account_uuid: None,
            },
            chrono::Utc::now(),
        );
        let model = gents_inference::clients::claude_subscription::default_model_name();
        let first = connect_first_sign_in(&access, OWNER, &signed_in).await;
        let again = connect_first_sign_in(&access, OWNER, &signed_in).await;

        let expected = Saved {
            notice: format!("saved anthropic-3: {model}"),
            profile: "profile-anthropic-3".to_string(),
        };
        assert_eq!(first.unwrap(), Some(expected));
        assert_eq!(again.unwrap(), None);
        let backends_now = stored(&access, Collection::InferenceBackend).await;
        let subscription = by(&backends_now, "backend_id", "anthropic-3");
        let reads = [&subscription["provider_kind"], &subscription["auth"]];
        let node_oauth = json!({ "kind": "node_oauth" });
        assert_eq!(reads, [&json!("ClaudeCliSubscription"), &node_oauth]);
        let profiles_now = stored(&access, Collection::InferenceProfile).await;
        let profile = by(&profiles_now, "profile_id", "profile-anthropic-3");
        let bound = [&profile["model_name"], &profile["backend_id"]];
        assert_eq!(bound, [&json!(model), &json!("anthropic-3")]);
        assert_eq!(stored(&access, Collection::Agent).await, agents);
        served.node.shutdown().await.expect("node shutdown");
    }

    /// The shell takes a whole number as the desktop's `Number()` does.
    #[test]
    fn a_whole_number_written_with_a_point_saves_as_an_integer() {
        let spec = connection_spec(
            InferenceProviderId::Local,
            InferenceAuthMethod::OptionalApiKey,
            "",
        )
        .unwrap();
        let document = |concurrency: &str| {
            let settings = [(SettingKey::MaxConcurrent, concurrency.to_string())];
            let auth = BackendAuth::Unauthenticated;
            backend_document("owner", "local", &spec, auth, "qwen", &settings)
        };

        let saved = document("4.0").unwrap();

        assert_eq!(saved.inference_backends[0].max_concurrent, Some(4));
        let unsampled = saved.inference_profiles[0].sampling_id.is_none();
        assert!(unsampled && saved.inference_sampling.is_empty());
        let refused = document("4.5").unwrap_err();
        assert_eq!(refused.to_string(), "Concurrency must be a whole number");
    }

    /// Refuses one request, echoing `key` in its body as some providers do.
    async fn echoing_refusal(key: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let body = format!(r#"{{"error":"Incorrect API key provided: {key}"}}"#);
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = stream.read(&mut [0; 4096]).await;
            let length = body.len();
            let answer = format!(
                "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n{body}"
            );
            stream.write_all(answer.as_bytes()).await.unwrap();
        });
        endpoint
    }

    #[tokio::test]
    async fn errors_never_carry_the_key() {
        let unreachable = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
        let access = ConfigAccess::Graphql(unreachable);
        let echoed = api_key("openai", &echoing_refusal("sk-secret").await, "sk-secret");
        let connection = api_key("openai", "http://127.0.0.1:9/v1", "sk-secret");
        // No store write here echoes its input, so save's answer is given an error that does.
        let echoing = anyhow::anyhow!("the node refused the backend with key sk-secret");

        let refused = discover(&access, "owner", &echoed).await;
        let found = discover(&access, "owner", &connection).await;
        let saved = saved_answer(Err(echoing), "gpt-x", &connection);

        let failure = |found: Result<Discovered, String>| match found {
            Ok(found) => found.failure,
            Err(error) => Some(error),
        };
        let texts = [failure(refused), failure(found), saved.err()];
        let texts: Vec<_> = texts.into_iter().flatten().collect();
        assert_eq!(texts.len(), 3, "each fails: {texts:?}");
        assert!(
            texts[0].contains("Incorrect API key provided"),
            "{}",
            texts[0]
        );
        for text in texts {
            assert!(!text.contains("sk-secret"), "{text}");
        }
    }
}
