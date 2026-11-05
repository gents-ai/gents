use std::io::{IsTerminal, Write as _};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::cli::args::{BackendPresetArg, InitArgs};

const OPENAI_DEFAULT_MODEL: &str = "gpt-5.4-mini";
const OLLAMA_DEFAULT_URL: &str = "http://127.0.0.1:11434/v1";
const LOCAL_PROBE_URLS: [&str; 2] = ["http://127.0.0.1:8080/v1", "http://127.0.0.1:11434/v1"];

struct BackendSelection {
    inference_url: Option<String>,
    backend_preset: Option<BackendPresetArg>,
    model_name: Option<String>,
    api_key: Option<String>,
    label: String,
}

impl BackendSelection {
    fn apply_to(self, args: &mut InitArgs) {
        args.inference_endpoint = self.inference_url;
        args.backend_preset = self.backend_preset;
        args.model_name = self.model_name;
        args.api_key = self.api_key;
    }
}

pub(crate) async fn resolve_backend_interactively(args: &mut InitArgs) -> Result<()> {
    if !should_prompt(args) {
        return Ok(());
    }
    let selection = pick_backend().await?;
    eprintln!("Using {}.\n", selection.label);
    selection.apply_to(args);
    Ok(())
}

fn should_prompt(args: &InitArgs) -> bool {
    !args.identity_only
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && !has_backend_arg(args)
        && !inference_endpoint_env_set()
}

fn has_backend_arg(args: &InitArgs) -> bool {
    args.resolved_inference_endpoint().is_some()
        || args.backend_preset.is_some()
        || args.provider_kind.is_some()
        || args.openai_wire_api.is_some()
        || args.api_key.is_some()
        || args.api_key_env_var.is_some()
        || args.model_name.is_some()
}

fn inference_endpoint_env_set() -> bool {
    std::env::var("INFERENCE_ENDPOINT")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

async fn pick_backend() -> Result<BackendSelection> {
    let local = detect_local_server().await;
    eprintln!("How do you want to run inference?");
    eprintln!("  1) OpenAI API key   (paste it; stored in the backend document)");
    match &local {
        Some((url, _)) => eprintln!("  2) local server     (detected at {url})"),
        None => eprintln!("  2) local server     (e.g. ollama / llama-server)"),
    }
    eprintln!("  3) custom URL");
    eprintln!("  4) ChatGPT / Codex subscription   (uses your ChatGPT plan; OAuth setup included)");
    eprintln!("  5) Grok subscription (SuperGrok / X Premium+)   (OAuth setup included)");
    eprintln!("  6) Claude subscription (Claude Pro / Max)   (OAuth setup included)");
    let choice = prompt_line("> ").await?;
    match choice.trim() {
        "1" | "" => {
            let key = prompt_secret("Paste your OpenAI API key (hidden): ").await?;
            let key = key.trim();
            if key.is_empty() {
                bail!("no API key entered");
            }
            let model = prompt_line_default("Model", OPENAI_DEFAULT_MODEL).await?;
            Ok(BackendSelection {
                inference_url: None,
                backend_preset: Some(BackendPresetArg::OpenAi),
                model_name: Some(model.clone()),
                api_key: Some(key.to_string()),
                label: format!("openai · {model}"),
            })
        }
        "2" => {
            let (url, detected_model) = match local {
                Some(found) => found,
                None => {
                    let url =
                        prompt_line_default("Local server base URL", OLLAMA_DEFAULT_URL).await?;
                    let detected = probe_or_warn(&url).await.unwrap_or_default();
                    (url, detected)
                }
            };
            let model = if detected_model.trim().is_empty() {
                prompt_line("Model name: ").await?.trim().to_string()
            } else {
                detected_model
            };
            if model.is_empty() {
                bail!("no model name entered");
            }
            Ok(BackendSelection {
                label: format!("{url} · {model}"),
                inference_url: Some(url),
                backend_preset: None,
                model_name: Some(model),
                api_key: None,
            })
        }
        "3" => {
            let url = prompt_line("Backend base URL (incl. /v1): ").await?;
            let url = url.trim().to_string();
            if url.is_empty() {
                bail!("no URL entered");
            }
            probe_or_warn(&url).await;
            let model = prompt_line("Model name: ").await?.trim().to_string();
            if model.is_empty() {
                bail!("no model name entered");
            }
            Ok(BackendSelection {
                label: format!("{url} · {model}"),
                inference_url: Some(url),
                backend_preset: None,
                model_name: Some(model),
                api_key: None,
            })
        }
        "4" => Ok(BackendSelection {
            inference_url: None,
            backend_preset: Some(BackendPresetArg::ChatGptCodex),
            model_name: None,
            api_key: None,
            label: "ChatGPT / Codex subscription".to_string(),
        }),
        "5" => Ok(BackendSelection {
            inference_url: None,
            backend_preset: Some(BackendPresetArg::XaiGrokOAuth),
            model_name: None,
            api_key: None,
            label: "Grok subscription (SuperGrok / X Premium+)".to_string(),
        }),
        "6" => Ok(BackendSelection {
            inference_url: None,
            backend_preset: Some(BackendPresetArg::ClaudeCliSubscription),
            model_name: None,
            api_key: None,
            label: "Claude subscription (Claude Pro / Max)".to_string(),
        }),
        other => bail!("unrecognized choice: {other}"),
    }
}

async fn detect_local_server() -> Option<(String, String)> {
    for url in LOCAL_PROBE_URLS {
        if let Some(model) = probe_models(url).await {
            return Some((url.to_string(), model));
        }
    }
    None
}

/// A model that does not answer yet is no reason to stop setup, but a mistyped address
/// would otherwise show only when the first window fails to open.
async fn probe_or_warn(url: &str) -> Option<String> {
    let model = probe_models(url).await;
    if model.is_none() {
        eprintln!(
            "No model answered at {url}. Setup will continue, but agents cannot run until one does: start it, or change the address later with `gents config backend set`."
        );
    }
    model
}

/// Discovery probes optional local endpoints before prompting; an absent or
/// unresponsive endpoint must not hold the interactive setup indefinitely.
async fn probe_models(base: &str) -> Option<String> {
    let response = reqwest::Client::new()
        .get(format!("{}/models", base.trim_end_matches('/')))
        .timeout(Duration::from_millis(700))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    body.pointer("/data/0/id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

pub(crate) async fn confirm(question: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    eprint!("{question} {hint} ");
    let _ = std::io::stderr().flush();
    match read_line().await {
        Ok(line) => match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default_yes,
        },
        Err(_) => default_yes,
    }
}

pub(crate) async fn prompt_line(text: &str) -> Result<String> {
    eprint!("{text}");
    let _ = std::io::stderr().flush();
    read_line().await
}

pub(crate) async fn prompt_line_default(label: &str, default: &str) -> Result<String> {
    eprint!("{label} [{default}]: ");
    let _ = std::io::stderr().flush();
    let line = read_line().await?;
    let trimmed = line.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
}

async fn prompt_secret(text: &str) -> Result<String> {
    eprint!("{text}");
    let _ = std::io::stderr().flush();
    // `biased` installs the handler on the first poll, before echo goes off, so no
    // interrupt can land while the terminal is hidden with nothing to restore it.
    let line = tokio::select! {
        biased;
        Ok(()) = tokio::signal::ctrl_c() => {
            set_terminal_echo(true);
            eprintln!();
            std::process::exit(130);
        }
        line = read_hidden_line() => line,
    };
    // Tokio never removes the handler, so without a listener Ctrl-C would do nothing for the
    // rest of the process. An interface in raw mode reads it as a key and no signal is raised.
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!();
            std::process::exit(130);
        }
    });
    line
}

async fn read_hidden_line() -> Result<String> {
    let hidden = set_terminal_echo(false);
    let line = read_line().await;
    if hidden {
        set_terminal_echo(true);
        eprintln!();
    }
    line
}

fn set_terminal_echo(on: bool) -> bool {
    std::process::Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn read_line() -> Result<String> {
    tokio::task::spawn_blocking(|| -> Result<String> {
        let mut buf = String::new();
        std::io::stdin()
            .read_line(&mut buf)
            .context("reading from stdin")?;
        Ok(buf)
    })
    .await
    .context("stdin reader task panicked")?
}
