use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use serde::Deserialize;

use crate::cli::args::CodexAuthProbeArgs;
use crate::config_writes::ConfigAccess;
use crate::{resolve_agent_did, resolve_config_access};

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    models: Vec<ModelSummary>,
    #[serde(default)]
    data: Vec<OpenAiModelSummary>,
}

#[derive(Deserialize)]
struct ModelSummary {
    slug: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
struct OpenAiModelSummary {
    id: String,
}

pub(crate) async fn codex_auth_probe(args: CodexAuthProbeArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let agent_did = resolve_agent_did(Some(&home_dir), args.agent_did.as_deref())?;
    let provider = normalize_provider(&args.provider);
    let credential = load_oauth_credential(&access, &agent_did, &provider)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(defra_agent::chatgpt_codex::classify_chatgpt_auth_error(
                &agent_did,
                &provider,
                &defra_agent::chatgpt_codex::ChatGptAuthProblem::Missing,
            ))
        })?;
    let credential = refresh_oauth_credential_if_needed(&access, credential).await?;

    let backend_url = defra_agent::chatgpt_codex::default_backend_endpoint();
    let models_url = format!("{}/models", backend_url.trim_end_matches('/'));
    let mut request = reqwest::Client::new()
        .get(&models_url)
        .query(&[("client_version", env!("CARGO_PKG_VERSION"))])
        .bearer_auth(&credential.access_token);
    for (name, value) in defra_agent::chatgpt_codex::build_chatgpt_codex_headers(
        credential.account_id.as_deref(),
        credential.is_fedramp,
    )? {
        if let Some(name) = name {
            request = request.header(name, value);
        }
    }

    let response = request
        .send()
        .await
        .context("failed to send models request to ChatGPT Codex backend")?;
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let body = response
        .bytes()
        .await
        .context("failed to read models response from ChatGPT Codex backend")?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&body);
        if status.as_u16() == 401 || status.as_u16() == 403 {
            let guidance = defra_agent::chatgpt_codex::classify_chatgpt_auth_error(
                &agent_did,
                &provider,
                &defra_agent::chatgpt_codex::ChatGptAuthProblem::Expired,
            );
            bail!("models request failed with HTTP {status}: {body}\n{guidance}");
        }
        bail!("models request failed with HTTP {status}: {body}");
    }
    let ModelsResponse { models, data } =
        serde_json::from_slice(&body).context("failed to decode models response")?;

    println!("Agent DID: {agent_did}");
    println!("Credential: {}", credential.credential_id);
    println!(
        "Auth: ChatGPT (account: {}, plan: {})",
        credential
            .account_id
            .as_deref()
            .unwrap_or("<unknown-account>"),
        credential.chatgpt_plan_type.as_deref().unwrap_or("Unknown")
    );
    println!("Backend: {backend_url}");
    println!(
        "Access token expires: {}",
        credential.access_token_expires_at
    );
    if let Some(etag) = etag {
        println!("Models etag: {etag}");
    }
    let mut rendered = models
        .into_iter()
        .map(|model| {
            let display_name = model.display_name.trim();
            if display_name.is_empty() || display_name == model.slug {
                model.slug
            } else {
                format!("{} ({display_name})", model.slug)
            }
        })
        .chain(data.into_iter().map(|model| model.id))
        .collect::<Vec<_>>();
    rendered.sort();
    println!("Models returned: {}", rendered.len());

    let max_models = args.max_models.min(rendered.len());
    for model in rendered.iter().take(max_models) {
        println!("- {model}");
    }

    if max_models < rendered.len() {
        println!("- ... {} more", rendered.len() - max_models);
    }

    Ok(())
}

pub(crate) async fn load_oauth_credential(
    access: &ConfigAccess,
    agent_did: &str,
    provider: &str,
) -> Result<Option<defra_agent::chatgpt_codex::OAuthCredential>> {
    let query = defra_agent::chatgpt_codex::oauth_credential_query(agent_did, provider);
    let response = access.execute(&query).await?;
    defra_agent::chatgpt_codex::oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub(crate) async fn refresh_oauth_credential_if_needed(
    access: &ConfigAccess,
    mut credential: defra_agent::chatgpt_codex::OAuthCredential,
) -> Result<defra_agent::chatgpt_codex::OAuthCredential> {
    if Utc::now() + Duration::minutes(5) < credential.access_token_expires_at {
        return Ok(credential);
    }
    let refreshed = defra_agent::chatgpt_oauth_refresh::refresh_chatgpt_token(
        &credential.refresh_token,
        &reqwest::Client::new(),
    )
    .await
    .map_err(|problem| {
        anyhow::anyhow!(defra_agent::chatgpt_codex::classify_chatgpt_auth_error(
            &credential.agent_did,
            &credential.provider,
            &problem,
        ))
    })?;
    credential.access_token = refreshed.access_token;
    credential.refresh_token = refreshed.refresh_token;
    if refreshed.id_token.is_some() {
        credential.id_token = refreshed.id_token;
    }
    if refreshed.account_id.is_some() {
        credential.account_id = refreshed.account_id;
    }
    if refreshed.plan_type.is_some() {
        credential.chatgpt_plan_type = refreshed.plan_type;
    }
    credential.is_fedramp = refreshed.is_fedramp || credential.is_fedramp;
    credential.access_token_expires_at = refreshed.access_token_expires_at;
    credential.last_refresh = Some(Utc::now());
    access
        .execute(&defra_agent::chatgpt_codex::oauth_credential_upsert_mutation(&credential))
        .await?;
    Ok(credential)
}

fn normalize_provider(provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() {
        defra_agent::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string()
    } else {
        provider.to_string()
    }
}
