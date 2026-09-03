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
                    let detected = probe_models(&url).await.unwrap_or_default();
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

async fn probe_models(base: &str) -> Option<String> {
    let response = reqwest::Client::new()
        .get(format!("{base}/models"))
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

async fn prompt_line(text: &str) -> Result<String> {
    eprint!("{text}");
    let _ = std::io::stderr().flush();
    read_line().await
}

async fn prompt_line_default(label: &str, default: &str) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::IdentityBackendArg;

    fn bare_init_args() -> InitArgs {
        InitArgs {
            home: None,
            data_dir: None,
            dangerously_overwrite: false,
            reset: false,
            identity_only: false,
            agent_name: "test-agent".to_string(),
            key_path: None,
            identity_backend: IdentityBackendArg::File,
            keychain_label: None,
            secure_enclave_label: None,
            inference_endpoint: None,
            backend_id: None,
            backend_name: None,
            backend_preset: None,
            provider_kind: None,
            openai_wire_api: None,
            api_key: None,
            api_key_env_var: None,
            model_name: None,
            max_concurrent: 2,
            max_queue_depth: 16,
            write_tools: false,
            yolo: false,
            tool_package: None,
            tool_root: None,
            enable_memory: false,
            disable_defra_query: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
        }
    }

    #[test]
    fn bare_invocation_has_no_backend_arg() {
        assert!(!has_backend_arg(&bare_init_args()));
    }

    #[test]
    fn each_backend_signal_is_detected() {
        let mut endpoint = bare_init_args();
        endpoint.inference_endpoint = Some("http://127.0.0.1:8080/v1".to_string());
        assert!(has_backend_arg(&endpoint));

        let mut preset = bare_init_args();
        preset.backend_preset = Some(BackendPresetArg::OpenAi);
        assert!(has_backend_arg(&preset));

        let mut provider_kind = bare_init_args();
        provider_kind.provider_kind = Some("Anthropic".to_string());
        assert!(has_backend_arg(&provider_kind));

        let mut wire_api = bare_init_args();
        wire_api.openai_wire_api = Some(crate::cli::args::OpenAiWireApiArg::Responses);
        assert!(has_backend_arg(&wire_api));

        let mut api_key = bare_init_args();
        api_key.api_key = Some("sk-test".to_string());
        assert!(has_backend_arg(&api_key));

        let mut api_key_env = bare_init_args();
        api_key_env.api_key_env_var = Some("OPENAI_API_KEY".to_string());
        assert!(has_backend_arg(&api_key_env));

        let mut model = bare_init_args();
        model.model_name = Some("gpt-5.4-mini".to_string());
        assert!(has_backend_arg(&model));
    }

    #[test]
    fn selection_overwrites_backend_fields_only() {
        let mut args = bare_init_args();
        args.agent_name = "keep-me".to_string();
        args.enable_memory = true;
        BackendSelection {
            inference_url: None,
            backend_preset: Some(BackendPresetArg::OpenAi),
            model_name: Some("gpt-5.4-mini".to_string()),
            api_key: Some("sk-test".to_string()),
            label: "openai · gpt-5.4-mini".to_string(),
        }
        .apply_to(&mut args);

        assert_eq!(args.backend_preset, Some(BackendPresetArg::OpenAi));
        assert_eq!(args.model_name.as_deref(), Some("gpt-5.4-mini"));
        assert_eq!(args.api_key.as_deref(), Some("sk-test"));
        assert_eq!(args.inference_endpoint, None);
        assert_eq!(args.agent_name, "keep-me");
        assert!(args.enable_memory);
    }

    #[test]
    fn codex_subscription_selection_seeds_keyless_preset() {
        let mut args = bare_init_args();
        BackendSelection {
            inference_url: None,
            backend_preset: Some(BackendPresetArg::ChatGptCodex),
            model_name: None,
            api_key: None,
            label: "ChatGPT / Codex subscription".to_string(),
        }
        .apply_to(&mut args);

        assert_eq!(args.backend_preset, Some(BackendPresetArg::ChatGptCodex));
        assert_eq!(args.api_key, None);
        assert_eq!(args.model_name, None);
        assert_eq!(args.inference_endpoint, None);
    }
}
