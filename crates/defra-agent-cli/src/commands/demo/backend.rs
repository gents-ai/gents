//! Backend resolution for the demo.
//!
//! Non-interactive paths (flags / `OPENAI_API_KEY`) win first; otherwise a
//! first-run picker offers an OpenAI key, a detected local server, or a custom
//! URL. The resolved `init` flags are persisted so a resumed session and a
//! paired node B reuse the same backend. No mock is ever offered.

use std::path::Path;

use anyhow::{bail, Result};

use crate::cli::args::DemoArgs;

use super::util::{non_empty, prompt_line, prompt_secret, StdinLines};

/// How the demo's inference backend was resolved, ready to pass to `init`.
#[derive(Clone)]
pub(super) struct BackendChoice {
    pub(super) init_args: Vec<String>,
    pub(super) label: String,
}

pub(super) async fn resolve_backend(
    args: &DemoArgs,
    reader: &mut StdinLines,
) -> Result<BackendChoice> {
    // Non-interactive paths first (flags / env).
    if let Some(url) = &args.inference_url {
        return Ok(custom_url_backend(
            url,
            args.model.as_deref(),
            args.api_key.as_deref(),
        ));
    }
    if let Some(preset) = &args.backend_preset {
        return Ok(preset_backend(
            preset,
            args.model.as_deref(),
            args.api_key.as_deref(),
        ));
    }
    if args.api_key.is_some() || std::env::var("OPENAI_API_KEY").is_ok() {
        return Ok(openai_backend(
            args.model.as_deref(),
            args.api_key.as_deref(),
        ));
    }
    // Interactive first-run picker.
    pick_backend(args.model.as_deref(), reader).await
}

/// Interactively pick a backend. Reused by first-run setup and `reconfigure`.
pub(super) async fn pick_backend(
    model: Option<&str>,
    reader: &mut StdinLines,
) -> Result<BackendChoice> {
    let local = detect_local().await;
    println!("\nHow do you want to run inference for the demo?");
    println!("  1) OpenAI API key   (paste it; stored locally in the demo home)");
    if let Some((url, _)) = &local {
        println!("  2) local server     (detected at {url})");
    } else {
        println!("  2) local server     (e.g. ollama / llama-server)");
    }
    println!("  3) custom URL");
    let choice = prompt_line(reader, "> ").await?;
    match choice.trim() {
        "1" | "" => {
            let key = prompt_secret(reader, "Paste your OpenAI API key (hidden): ").await?;
            let key = key.trim();
            if key.is_empty() {
                bail!("no API key entered");
            }
            Ok(openai_backend(model, Some(key)))
        }
        "2" => {
            let (url, detected_model) = match local {
                Some(found) => found,
                None => {
                    let url = prompt_line(
                        reader,
                        "Local server base URL [http://127.0.0.1:11434/v1]: ",
                    )
                    .await?;
                    let url = non_empty(&url)
                        .unwrap_or("http://127.0.0.1:11434/v1")
                        .to_string();
                    let detected = crate::onboarding::probe_models(&url)
                        .await
                        .unwrap_or_default();
                    (url, detected)
                }
            };
            let model = model
                .map(str::to_string)
                .or_else(|| non_empty(&detected_model).map(str::to_string));
            Ok(custom_url_backend(&url, model.as_deref(), None))
        }
        "3" => {
            let url = prompt_line(reader, "Backend base URL (incl. /v1): ").await?;
            let url = url.trim();
            if url.is_empty() {
                bail!("no URL entered");
            }
            let model_name = prompt_line(reader, "Model name: ").await?;
            Ok(custom_url_backend(url, non_empty(&model_name), None))
        }
        other => bail!("unrecognized choice: {other}"),
    }
}

async fn detect_local() -> Option<(String, String)> {
    // Shared detection so the demo and the first-class `onboard` flow probe the
    // same local endpoints and agree on what "a local server" means (#647).
    crate::onboarding::detect_local_backend()
        .await
        .map(|backend| (backend.url, backend.model))
}

fn openai_backend(model: Option<&str>, api_key: Option<&str>) -> BackendChoice {
    let model = model.unwrap_or("gpt-4.1-mini").to_string();
    let mut init_args = vec![
        "--backend-preset".into(),
        "openai".into(),
        "--model-name".into(),
        model.clone(),
    ];
    if let Some(key) = api_key {
        init_args.push("--api-key".into());
        init_args.push(key.to_string());
    }
    BackendChoice {
        init_args,
        label: format!("openai · {model}"),
    }
}

fn preset_backend(preset: &str, model: Option<&str>, api_key: Option<&str>) -> BackendChoice {
    let model = model.unwrap_or("gpt-4.1-mini").to_string();
    let mut init_args = vec![
        "--backend-preset".into(),
        preset.to_string(),
        "--model-name".into(),
        model.clone(),
    ];
    if let Some(key) = api_key {
        init_args.push("--api-key".into());
        init_args.push(key.to_string());
    }
    BackendChoice {
        init_args,
        label: format!("{preset} · {model}"),
    }
}

fn custom_url_backend(url: &str, model: Option<&str>, api_key: Option<&str>) -> BackendChoice {
    let model = model.unwrap_or("demo-model").to_string();
    let mut init_args = vec![
        "--inference-url".into(),
        url.to_string(),
        "--model-name".into(),
        model.clone(),
    ];
    if let Some(key) = api_key {
        init_args.push("--api-key".into());
        init_args.push(key.to_string());
    }
    BackendChoice {
        init_args,
        label: format!("{url} · {model}"),
    }
}

pub(super) fn write_backend_args(path: &Path, args: &[String]) {
    if let Ok(json) = serde_json::to_string(args) {
        let _ = std::fs::write(path, json);
    }
}

pub(super) fn read_backend_args(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}
