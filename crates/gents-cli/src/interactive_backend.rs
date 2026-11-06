use std::io::{IsTerminal, Write as _};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents::inference::backends::setup::{
    inference_setup_catalog, InferenceAuthMethod, InferenceAuthOption, InferenceProviderId,
};
use serde_json::Value;

use crate::cli::args::{BackendPresetArg, InitArgs, OpenAiWireApiArg};

const OPENAI_DEFAULT_MODEL: &str = "gpt-5.4-mini";
const LOCAL_PROBE_URLS: [&str; 2] = ["http://127.0.0.1:8080/v1", "http://127.0.0.1:11434/v1"];

/// The menu's answer, kept so `init` can list the connection's models once it is signed in.
pub(crate) struct PickedConnection {
    pub(crate) provider: InferenceProviderId,
    pub(crate) method: InferenceAuthMethod,
    pub(crate) provider_name: String,
    pub(crate) endpoint: String,
    pub(crate) api_key: Option<String>,
    pub(crate) fallback_model: Option<String>,
    pub(crate) next_screen: fn(),
}

struct BackendSelection {
    inference_url: Option<String>,
    backend_preset: Option<BackendPresetArg>,
    openai_wire_api: Option<OpenAiWireApiArg>,
    api_key: Option<String>,
    label: String,
    connection: PickedConnection,
}

impl BackendSelection {
    fn apply_to(self, args: &mut InitArgs) {
        args.inference_endpoint = self.inference_url;
        args.backend_preset = self.backend_preset;
        args.openai_wire_api = self.openai_wire_api;
        args.api_key = self.api_key;
        args.picked_connection = Some(self.connection);
    }
}

/// `next_screen` runs after each answer, so whatever follows that answer is on the screen it
/// leaves: a warning stays with the question after it.
pub(crate) async fn resolve_backend_interactively(
    args: &mut InitArgs,
    next_screen: fn(),
) -> Result<()> {
    if !should_prompt(args) {
        return Ok(());
    }
    let selection = pick_backend(next_screen).await?;
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

/// Asks what the desktop's inference step asks, from the catalog it reads, so a provider
/// appears here once whichever way it is reached.
async fn pick_backend(next_screen: fn()) -> Result<BackendSelection> {
    let local = detect_local_server().await;
    let catalog = inference_setup_catalog();
    eprintln!("Choose an inference provider");
    for (number, provider) in (1..).zip(&catalog.providers) {
        let detected = match (&local, provider.id) {
            (Some(url), InferenceProviderId::Local) => format!(" Detected at {url}."),
            _ => String::new(),
        };
        eprintln!(
            "  {number}) {:<12}{}{detected}",
            provider.display_name, provider.description
        );
    }
    let provider = &catalog.providers[ask_choice(catalog.providers.len()).await?];
    next_screen();
    let option = match provider.auth_options.as_slice() {
        [only] => only,
        options => {
            eprintln!("Choose a connection method for {}", provider.display_name);
            for (number, option) in (1..).zip(options) {
                eprintln!("  {number}) {}", option.display_name);
            }
            let chosen = &options[ask_choice(options.len()).await?];
            next_screen();
            chosen
        }
    };
    let connection = PickedConnection {
        provider: provider.id,
        method: option.method,
        provider_name: provider.display_name.clone(),
        endpoint: String::new(),
        api_key: None,
        fallback_model: None,
        next_screen,
    };
    match connection_preset(provider.id, option.method) {
        None => local_selection(option, local, connection).await,
        Some(preset) if option.method == InferenceAuthMethod::ApiKey => {
            api_key_selection(preset, option, connection).await
        }
        Some(preset) => Ok(BackendSelection {
            inference_url: None,
            backend_preset: Some(preset),
            openai_wire_api: connection_wire_api(option.method),
            api_key: None,
            label: option.display_name.clone(),
            connection: PickedConnection {
                fallback_model: preset.default_model_name().map(str::to_string),
                ..connection
            },
        }),
    }
}

/// A wrong answer is not repeated back: a key pasted at this prompt must not reach scrollback
/// a second time.
async fn ask_choice(count: usize) -> Result<usize> {
    loop {
        match parse_choice(&prompt_line("> ").await?, count) {
            Some(index) => return Ok(index),
            None => eprintln!("Answer 1 to {count}."),
        }
    }
}

fn parse_choice(answer: &str, count: usize) -> Option<usize> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Some(0);
    }
    let number = answer.parse::<usize>().ok()?;
    (1..=count).contains(&number).then(|| number - 1)
}

/// `None` is the local server, which is reached by its address and has no preset.
fn connection_preset(
    provider: InferenceProviderId,
    method: InferenceAuthMethod,
) -> Option<BackendPresetArg> {
    use InferenceAuthMethod::{ApiKey, ChatGptOauth, ClaudeOauth, GrokOauth};
    match (provider, method) {
        (InferenceProviderId::OpenAi, ChatGptOauth) => Some(BackendPresetArg::ChatGptCodex),
        (InferenceProviderId::OpenAi, ApiKey) => Some(BackendPresetArg::OpenAi),
        (InferenceProviderId::Anthropic, ClaudeOauth) => {
            Some(BackendPresetArg::ClaudeCliSubscription)
        }
        (InferenceProviderId::Anthropic, ApiKey) => Some(BackendPresetArg::Anthropic),
        (InferenceProviderId::Grok, GrokOauth) => Some(BackendPresetArg::XaiGrokOAuth),
        (InferenceProviderId::Grok, ApiKey) => Some(BackendPresetArg::XaiApiKey),
        (InferenceProviderId::OpenRouter, ApiKey) => Some(BackendPresetArg::OpenRouter),
        _ => None,
    }
}

/// The desktop sends Grok sign-in over Chat Completions, where the `xai-oauth` preset's own
/// default is Responses.
fn connection_wire_api(method: InferenceAuthMethod) -> Option<OpenAiWireApiArg> {
    (method == InferenceAuthMethod::GrokOauth).then_some(OpenAiWireApiArg::ChatCompletions)
}

async fn api_key_selection(
    preset: BackendPresetArg,
    option: &InferenceAuthOption,
    connection: PickedConnection,
) -> Result<BackendSelection> {
    eprintln!("The key is stored in the backend document.");
    let key = prompt_secret(&format!("Paste your {} (hidden): ", option.display_name)).await?;
    let key = key.trim().to_string();
    if key.is_empty() {
        bail!("no API key entered");
    }
    (connection.next_screen)();
    Ok(BackendSelection {
        inference_url: None,
        backend_preset: Some(preset),
        openai_wire_api: None,
        api_key: Some(key.clone()),
        label: option.display_name.clone(),
        connection: PickedConnection {
            api_key: Some(key),
            fallback_model: matches!(preset, BackendPresetArg::OpenAi)
                .then(|| OPENAI_DEFAULT_MODEL.to_string()),
            ..connection
        },
    })
}

async fn local_selection(
    option: &InferenceAuthOption,
    detected: Option<String>,
    connection: PickedConnection,
) -> Result<BackendSelection> {
    let default_url = detected.unwrap_or_else(|| option.default_endpoint.clone());
    let url = prompt_line_default("Server base URL", &default_url).await?;
    (connection.next_screen)();
    Ok(BackendSelection {
        label: url.clone(),
        inference_url: Some(url.clone()),
        backend_preset: None,
        openai_wire_api: None,
        api_key: None,
        connection: PickedConnection {
            endpoint: url,
            ..connection
        },
    })
}

async fn detect_local_server() -> Option<String> {
    for url in LOCAL_PROBE_URLS {
        if probe_models(url).await.is_some() {
            return Some(url.to_string());
        }
    }
    None
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
        Err(_) => false,
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

/// End of input (Ctrl-D or a closed stdin) is an error: as an empty line it would accept
/// whatever the prompt offers by default.
async fn read_line() -> Result<String> {
    tokio::task::spawn_blocking(|| -> Result<String> {
        let mut buf = String::new();
        let read = std::io::stdin()
            .read_line(&mut buf)
            .context("reading from stdin")?;
        if read == 0 {
            eprintln!();
            bail!("input ended before an answer; run this again to continue");
        }
        Ok(buf)
    })
    .await
    .context("stdin reader task panicked")?
}

#[cfg(test)]
mod tests {
    use gents::inference::backends::setup::connection_spec;
    use gents::OpenAiWireApi;

    use super::*;
    use crate::{resolve_backend_config_with_preset, BackendResolutionMode};

    #[test]
    fn an_empty_answer_takes_the_first_choice() {
        assert_eq!(parse_choice("", 5), Some(0));
        assert_eq!(parse_choice("  \n", 5), Some(0));
    }

    #[test]
    fn only_a_number_on_the_list_chooses() {
        assert_eq!(parse_choice("1", 2), Some(0));
        assert_eq!(parse_choice("2\n", 2), Some(1));
        for garbage in ["0", "3", "sk-pasted", "1 2", "openai", "-1"] {
            assert_eq!(parse_choice(garbage, 2), None, "{garbage:?}");
        }
    }

    #[test]
    fn every_catalog_connection_resolves_as_the_desktop_resolves_it() {
        for provider in inference_setup_catalog().providers {
            for option in provider.auth_options {
                let spec = connection_spec(provider.id, option.method, "").unwrap();
                let Some(preset) = connection_preset(provider.id, option.method) else {
                    assert_eq!(provider.id, InferenceProviderId::Local);
                    continue;
                };
                let resolved = resolve_backend_config_with_preset(
                    Some(preset),
                    None,
                    None,
                    connection_wire_api(option.method),
                    spec.api_key_required.then_some("key"),
                    None,
                    BackendResolutionMode::ConfigWrite,
                )
                .unwrap();
                let name = &option.display_name;
                assert_eq!(resolved.provider_kind, spec.provider_kind, "{name}");
                assert_eq!(
                    OpenAiWireApi::effective_for_provider(
                        resolved.provider_kind,
                        resolved.openai_wire_api
                    ),
                    OpenAiWireApi::effective_for_provider(spec.provider_kind, spec.openai_wire_api),
                    "{name}"
                );
                assert_eq!(resolved.endpoint, spec.endpoint, "{name}");
            }
        }
    }
}
