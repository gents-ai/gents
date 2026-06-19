use anyhow::{bail, Context, Result};
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use serde::Deserialize;

use crate::cli::args::CodexAuthProbeArgs;

#[derive(Deserialize)]
struct ModelsResponse {
    models: Vec<ModelSummary>,
}

#[derive(Deserialize)]
struct ModelSummary {
    slug: String,
    #[serde(default)]
    display_name: String,
}

pub(crate) async fn codex_auth_probe(args: CodexAuthProbeArgs) -> Result<()> {
    let codex_home = defra_agent::chatgpt_codex::resolve_codex_home(args.codex_home)?;
    let (_manager, auth) = match defra_agent::chatgpt_codex::resolve_chatgpt_auth(&codex_home).await
    {
        Ok(resolved) => resolved,
        Err(problem) => {
            let guidance =
                defra_agent::chatgpt_codex::classify_chatgpt_auth_error(&codex_home, &problem);
            anyhow::bail!("{guidance}");
        }
    };

    let account_email = auth
        .get_account_email()
        .unwrap_or_else(|| "<unknown-email>".to_string());
    let plan = auth
        .account_plan_type()
        .map(|plan| format!("{plan:?}"))
        .unwrap_or_else(|| "Unknown".to_string());

    let backend_url = CHATGPT_CODEX_BASE_URL;
    let models_url = format!("{}/models", backend_url.trim_end_matches('/'));
    let access_token = auth
        .get_token()
        .context("ChatGPT auth did not expose a bearer token")?;

    let mut request = reqwest::Client::new()
        .get(models_url)
        .query(&[("client_version", env!("CARGO_PKG_VERSION"))])
        .bearer_auth(access_token);
    for (name, value) in defra_agent::chatgpt_codex::build_chatgpt_codex_headers(&auth)? {
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
                &codex_home,
                &defra_agent::chatgpt_codex::ChatGptAuthProblem::Expired,
            );
            bail!("models request failed with HTTP {status}: {body}\n{guidance}");
        }
        bail!("models request failed with HTTP {status}: {body}");
    }
    let ModelsResponse { models } =
        serde_json::from_slice(&body).context("failed to decode models response")?;

    println!("Codex home: {}", codex_home.display());
    println!("Auth: ChatGPT ({account_email}, plan: {plan})");
    println!("Backend: {backend_url}");
    if let Some(etag) = etag {
        println!("Models etag: {etag}");
    }
    println!("Models returned: {}", models.len());

    let max_models = args.max_models.min(models.len());
    for model in models.iter().take(max_models) {
        let display_name = model.display_name.trim();
        if display_name.is_empty() || display_name == model.slug {
            println!("- {}", model.slug);
        } else {
            println!("- {} ({display_name})", model.slug);
        }
    }

    if max_models < models.len() {
        println!("- ... {} more", models.len() - max_models);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_missing_home_is_actionable() {
        let tmp = std::env::temp_dir().join("defra-codex-probe-missing-xyz");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let args = CodexAuthProbeArgs {
            codex_home: Some(tmp.clone()),
            max_models: 5,
        };
        let err = codex_auth_probe(args)
            .await
            .expect_err("no auth in empty home");
        let msg = format!("{err:#}");

        assert!(msg.contains("codex login"), "actionable: {msg}");
    }
}
