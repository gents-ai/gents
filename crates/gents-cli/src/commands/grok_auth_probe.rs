use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::cli::args::GrokAuthProbeArgs;
use crate::config_writes::ConfigAccess;
use crate::{resolve_agent_did, resolve_config_access};

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    models: Vec<ModelSummary>,
    #[serde(default)]
    data: Vec<ModelSummary>,
}

/// One catalog entry, tolerant of every shape the proxy is known to serve:
/// official `/models-v2` entries (`model` / `modelId` / `name`; `id` is a row
/// id), plus OpenAI-style (`id`) and slug-style (`slug` / `display_name`).
#[derive(Deserialize)]
struct ModelSummary {
    #[serde(default)]
    model: String,
    #[serde(default, rename = "modelId")]
    model_id: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    display_name: String,
}

impl ModelSummary {
    fn rendered(self) -> Option<String> {
        let id = [self.model, self.model_id, self.slug, self.id]
            .into_iter()
            .find(|value| !value.trim().is_empty())?;
        let display_name = if !self.display_name.trim().is_empty() {
            self.display_name
        } else {
            self.name
        };
        let display_name = display_name.trim();
        if display_name.is_empty() || display_name == id {
            Some(id)
        } else {
            Some(format!("{id} ({display_name})"))
        }
    }
}

fn rendered_model_names(body: &[u8]) -> Result<Vec<String>> {
    let ModelsResponse { models, data } =
        serde_json::from_slice(body).context("failed to decode models response")?;
    let mut rendered = models
        .into_iter()
        .chain(data)
        .filter_map(ModelSummary::rendered)
        .collect::<Vec<_>>();
    rendered.sort();
    Ok(rendered)
}

pub(crate) async fn grok_auth_probe(args: GrokAuthProbeArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let agent_did = resolve_agent_did(Some(&home_dir), args.agent_did.as_deref())?;
    let provider = gents::xai_grok_oauth::normalize_provider(&args.provider);
    let credential = load_oauth_credential(&access, &agent_did, &provider)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(gents::xai_grok_oauth::classify_xai_auth_error(
                &agent_did,
                &provider,
                &gents::oauth_credential::OAuthAuthProblem::Missing,
            ))
        })?;

    let backend_url = gents::xai_grok_oauth::default_backend_endpoint();
    // `/models-v2` is the catalog path the official Grok CLI queries.
    let models_url = format!("{}/models-v2", backend_url.trim_end_matches('/'));
    let mut request = reqwest::Client::new()
        .get(&models_url)
        .bearer_auth(&credential.access_token);
    for (name, value) in gents::xai_grok_oauth::build_xai_grok_oauth_headers()? {
        if let Some(name) = name {
            request = request.header(name, value);
        }
    }

    let response = request
        .send()
        .await
        .context("failed to send models request to Grok OAuth backend")?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("failed to read models response from Grok OAuth backend")?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&body);
        if status.as_u16() == 401 {
            let guidance = gents::xai_grok_oauth::classify_xai_auth_error(
                &agent_did,
                &provider,
                &gents::oauth_credential::OAuthAuthProblem::Expired,
            );
            bail!("models request failed with HTTP {status}: {body}\n{guidance}");
        }
        if status.as_u16() == 403 {
            let guidance = gents::xai_grok_oauth::classify_xai_auth_error(
                &agent_did,
                &provider,
                &gents::oauth_credential::OAuthAuthProblem::NotEntitled,
            );
            bail!("models request failed with HTTP {status}: {body}\n{guidance}");
        }
        bail!("models request failed with HTTP {status}: {body}");
    }
    let rendered = rendered_model_names(&body)?;

    println!("Agent DID: {agent_did}");
    println!("Credential: {}", credential.credential_id);
    println!("Auth: Grok / xAI OAuth (subscription proxy)");
    println!("Backend: {backend_url}");
    println!(
        "Access token expires: {}",
        credential.access_token_expires_at
    );
    if !gents::oauth_credential::token_is_fresh(credential.access_token_expires_at) {
        println!(
            "Note: access token is within the refresh skew window; the owner runtime will refresh on next use."
        );
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_model_names_reads_models_v2_data_entries() {
        let body = br#"{"data":[{"id":"row-1","model":"grok-4.5","name":"Grok 4.5"},{"id":"row-2","modelId":"grok-build-0.1"}]}"#;
        let rendered = rendered_model_names(body).expect("models-v2 body parses");
        assert_eq!(rendered, vec!["grok-4.5 (Grok 4.5)", "grok-build-0.1"]);
    }

    #[test]
    fn rendered_model_names_still_reads_openai_and_slug_shapes() {
        let body = br#"{"models":[{"slug":"grok-4.5","display_name":"Grok 4.5"}],"data":[{"id":"grok-2"}]}"#;
        let rendered = rendered_model_names(body).expect("mixed body parses");
        assert_eq!(rendered, vec!["grok-2", "grok-4.5 (Grok 4.5)"]);
    }
}

pub(crate) async fn load_oauth_credential(
    access: &ConfigAccess,
    agent_did: &str,
    provider: &str,
) -> Result<Option<gents::oauth_credential::OAuthCredential>> {
    let query = gents::oauth_credential::oauth_credential_query(agent_did, provider);
    let response = access.execute(&query).await?;
    gents::oauth_credential::oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}
