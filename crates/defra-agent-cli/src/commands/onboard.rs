//! `onboard` — the interactive first-launch inference-backend flow (#647).
//!
//! Queries the stored inference backends, probes for a live local server, and
//! applies the #647 decision tree ([`crate::onboarding::plan_backend_onboarding`]):
//! a single stored backend is bound (and best-effort probed — see
//! [`connect_stored_backend`]); a detected local one is adopted; multiple
//! stored backends prompt for a pick; nothing usable offers to start a local
//! server (printing instructions to re-run) or configure a remote one. The
//! chosen backend AND its model are bound to the agent's default behavior.
//!
//! Config writes go through [`resolve_config_access`], so onboard works both
//! against a running server (its GraphQL endpoint) and offline (an embedded
//! node) — it never opens a second node on a data dir the server already holds.
//!
//! Model resolution mirrors `init`'s `resolve_init_model_name` so the two
//! commands never disagree on what model a preset/URL binds. Only `print_json`
//! writes to stdout; every human message goes to stderr, so scripted consumers
//! see a single JSON document.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use defra_agent::default_behavior_id_for_agent;
use defra_agent::graphql::escape_graphql_string;

use crate::cli::args::{BackendPresetArg, OnboardArgs};
use crate::config_writes::{
    write_inference_backend_document, ConfigAccess, InferenceBackendUpsertDocument,
};
use crate::onboarding::{
    detect_local_backend, plan_backend_onboarding, probe_models, BackendPlan, ConfiguredBackend,
    DetectedBackend,
};
use crate::prompt::{non_empty, prompt_line, prompt_secret, stdin_lines, StdinLines};
use crate::resolve_helpers::{default_backend_max_queue_depth, resolve_backend_config_with_preset};
use crate::shared::ResolvedBackendConfig;
use crate::{
    print_json, read_init_config, resolve_config_access, resolve_home_dir, BackendResolutionMode,
    DEFAULT_INIT_MODEL_NAME,
};

/// Matches init's `--max-concurrent` default so an onboard-configured backend
/// behaves like an init-configured one under the same canonical id.
const DEFAULT_MAX_CONCURRENT: i64 = 2;
/// A backend onboarding just probed successfully (detected-local / reachable
/// AutoConnect): safe to mark runnable now instead of waiting for the server's
/// startup re-probe.
const HEALTHY_PROBE_STATUS: &str = "healthy";
/// A backend onboarding did not measure (remote / preset / scripted): the
/// startup probe (#640) decides its health.
const UNKNOWN_PROBE_STATUS: &str = "unknown";

pub(crate) async fn onboard(args: OnboardArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let init_config = read_init_config(&home_dir)?.ok_or_else(|| {
        anyhow!(
            "no initialized agent in {}; run `defra-agent init` first",
            home_dir.display()
        )
    })?;
    let agent_did = init_config.agent_did.trim().to_string();
    if agent_did.is_empty() {
        anyhow::bail!("initialized home {} has no agent DID", home_dir.display());
    }
    let default_behavior_id = default_behavior_id_for_agent(&agent_did);

    // `--api-key`/`--api-key-env-var` alone cannot define a backend; require an
    // endpoint or preset so the key is never silently dropped.
    let has_backend_spec = args.backend_preset.is_some() || args.inference_url.is_some();
    if (args.api_key.is_some() || args.api_key_env_var.is_some()) && !has_backend_spec {
        anyhow::bail!("--api-key / --api-key-env-var require --backend-preset or --inference-url");
    }

    // Route through the running server when one is up, else an offline node —
    // never a second node on the server's locked data dir.
    let (access, _home) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;

    // Non-interactive escape hatch: explicit backend flags configure that
    // backend and bind it with no prompts (headless / CI / scripted onboarding).
    if has_backend_spec {
        let resolved = resolve_backend_config_with_preset(
            args.backend_preset,
            args.inference_url.as_deref(),
            None,
            None,
            args.api_key.as_deref(),
            args.api_key_env_var.as_deref(),
            BackendResolutionMode::Init,
        )?;
        let model = resolve_onboard_model(args.backend_preset, args.model.as_deref())?;
        let backend_id =
            persist_backend(&access, &agent_did, &resolved, &model, UNKNOWN_PROBE_STATUS).await?;
        bind_default_behavior(&access, &default_behavior_id, &backend_id, Some(&model)).await?;
        return report(&backend_id, "configured", &default_behavior_id);
    }

    let configured = query_configured_backends(&access).await?;
    let detected = detect_local_backend().await;
    let model_override = validated_model_override(args.model.as_deref())?;

    match plan_backend_onboarding(&configured, detected) {
        BackendPlan::AutoConnect { backend_id } => {
            connect_stored_backend(&access, &configured, &default_behavior_id, &backend_id).await
        }
        BackendPlan::AdoptDetected { detected } => {
            adopt_detected(
                &access,
                &agent_did,
                &default_behavior_id,
                &detected,
                model_override,
            )
            .await
        }
        BackendPlan::Select { backend_ids } => {
            let mut reader = stdin_lines();
            let backend_id = prompt_select_backend(&mut reader, &backend_ids).await?;
            connect_stored_backend(&access, &configured, &default_behavior_id, &backend_id).await
        }
        BackendPlan::OfferLaunchOrRemote => {
            let mut reader = stdin_lines();
            match offer_launch_or_remote(&mut reader).await? {
                // "Launch a local server": instructions were printed; nothing is
                // bound — the user re-runs `onboard` once the server is up.
                None => Ok(()),
                Some((resolved, model)) => {
                    let backend_id = persist_backend(
                        &access,
                        &agent_did,
                        &resolved,
                        &model,
                        UNKNOWN_PROBE_STATUS,
                    )
                    .await?;
                    bind_default_behavior(&access, &default_behavior_id, &backend_id, Some(&model))
                        .await?;
                    report(&backend_id, "configured", &default_behavior_id)
                }
            }
        }
    }
}

/// Bind an already-stored backend as the default behavior's backend (used by
/// AutoConnect and Select). Best-effort, NON-destructive: a successful
/// unauthenticated `/models` probe records the backend healthy so it is
/// immediately runnable; a failed probe (an authenticated remote returns 401,
/// or a slow/down endpoint) does NOT tear down the operator's configuration —
/// it binds the chosen backend anyway and explains how to reconfigure. The
/// runtime's own startup probe (which authenticates) still owns health for
/// endpoints this probe cannot reach.
async fn connect_stored_backend(
    access: &ConfigAccess,
    configured: &[ConfiguredBackend],
    behavior_id: &str,
    backend_id: &str,
) -> Result<()> {
    let endpoint = endpoint_for(configured, backend_id).map(str::to_string);
    let verified = match endpoint.as_deref() {
        Some(url) => probe_models(url).await.is_some(),
        None => false,
    };
    if verified {
        mark_backend_probe_status(access, backend_id, HEALTHY_PROBE_STATUS).await?;
    } else {
        let at = endpoint.map(|url| format!(" ({url})")).unwrap_or_default();
        eprintln!(
            "Bound {backend_id}{at}, but could not verify it with an unauthenticated probe \
             (an authenticated or slow remote is expected to fail this check). If it is the \
             wrong backend, reconfigure with --backend-preset/--inference-url, or disable it \
             and re-run onboard."
        );
    }
    // Keep the behavior's existing model_name — this is the configured backend.
    bind_default_behavior(access, behavior_id, backend_id, None).await?;
    report(backend_id, "connected", behavior_id)
}

/// Adopt a live local server detected by [`detect_local_backend`]: persist it
/// healthy (its `/models` was just probed) and bind it (with the operator's
/// `--model` override, else the model the server advertised).
async fn adopt_detected(
    access: &ConfigAccess,
    agent_did: &str,
    behavior_id: &str,
    detected: &DetectedBackend,
    model_override: Option<String>,
) -> Result<()> {
    let model = model_override.unwrap_or_else(|| detected.model.clone());
    let resolved = resolve_backend_config_with_preset(
        None,
        Some(&detected.url),
        None,
        None,
        None,
        None,
        BackendResolutionMode::Init,
    )?;
    let backend_id =
        persist_backend(access, agent_did, &resolved, &model, HEALTHY_PROBE_STATUS).await?;
    bind_default_behavior(access, behavior_id, &backend_id, Some(&model)).await?;
    eprintln!(
        "Detected a local inference server at {} — connected.",
        detected.url
    );
    report(&backend_id, "connected", behavior_id)
}

/// Resolve the model to bind for a flag-configured backend, mirroring init's
/// `resolve_init_model_name`: an explicit `--model` wins (rejected when empty),
/// else the preset's safe default, else an error for presets/URLs with no safe
/// default, else the init default for a bare `--inference-url`.
fn resolve_onboard_model(preset: Option<BackendPresetArg>, model: Option<&str>) -> Result<String> {
    if let Some(explicit) = model {
        let trimmed = explicit.trim();
        if trimmed.is_empty() {
            anyhow::bail!("--model must not be empty");
        }
        return Ok(trimmed.to_string());
    }
    if let Some(default) = preset.and_then(BackendPresetArg::default_model_name) {
        return Ok(default.to_string());
    }
    if let Some(preset) = preset {
        anyhow::bail!(
            "--model is required for --backend-preset {} because that preset has no safe default model",
            preset.as_str()
        );
    }
    Ok(DEFAULT_INIT_MODEL_NAME.to_string())
}

/// Validate an optional `--model` override for the detect path: reject a
/// whitespace-only value (consistent with init), pass through a trimmed value.
fn validated_model_override(model: Option<&str>) -> Result<Option<String>> {
    match model {
        Some(value) if value.trim().is_empty() => anyhow::bail!("--model must not be empty"),
        Some(value) => Ok(Some(value.trim().to_string())),
        None => Ok(None),
    }
}

/// Every configured backend (global, not per-agent): id, enabled, and endpoint
/// (used to probe an AutoConnect candidate for reachability).
async fn query_configured_backends(access: &ConfigAccess) -> Result<Vec<ConfiguredBackend>> {
    let response = access
        .execute("{ InferenceBackend { backend_id enabled endpoint } }")
        .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let backend_id = row.get("backend_id").and_then(Value::as_str)?;
            if backend_id.trim().is_empty() {
                return None;
            }
            Some(ConfiguredBackend {
                backend_id: backend_id.to_string(),
                // A backend with no explicit `enabled` counts as enabled.
                enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                endpoint: row
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect())
}

fn endpoint_for<'a>(configured: &'a [ConfiguredBackend], backend_id: &str) -> Option<&'a str> {
    configured
        .iter()
        .find(|backend| backend.backend_id == backend_id)
        .and_then(|backend| backend.endpoint.as_deref())
}

/// Persist a backend under the agent's canonical backend id (updating in place
/// so onboarding is idempotent) and return that id.
///
/// `probe_status` is `HEALTHY_PROBE_STATUS` only when onboarding just made a
/// successful live probe of this exact endpoint; otherwise
/// `UNKNOWN_PROBE_STATUS`, so the honest-probe subsystem (#640) — not this
/// write — owns promoting an unmeasured endpoint to healthy.
async fn persist_backend(
    access: &ConfigAccess,
    agent_did: &str,
    resolved: &ResolvedBackendConfig,
    model: &str,
    probe_status: &str,
) -> Result<String> {
    let backend_id = format!("{agent_did}:backend");
    let doc = InferenceBackendUpsertDocument {
        backend_id: backend_id.clone(),
        name: "Onboarded backend".to_string(),
        provider_kind: resolved.provider_kind,
        openai_wire_api: resolved.openai_wire_api,
        endpoint: resolved.endpoint.clone(),
        api_key: resolved.api_key.clone(),
        api_key_env_var: resolved.api_key_env_var.clone(),
        max_concurrent: DEFAULT_MAX_CONCURRENT,
        max_queue_depth: default_backend_max_queue_depth(),
        enabled: true,
        models_on_add: vec![model.to_string()],
        models_on_update: Some(vec![model.to_string()]),
        probe_status: probe_status.to_string(),
    };
    write_inference_backend_document(access, &doc).await?;
    Ok(backend_id)
}

/// Set an existing backend's `probe_status` (used to record a reachable
/// AutoConnect candidate healthy after a successful probe).
async fn mark_backend_probe_status(
    access: &ConfigAccess,
    backend_id: &str,
    status: &str,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{id}" }} }},
                input: {{ probe_status: "{status}" }}
            ) {{ _docID }}
        }}"#,
        id = escape_graphql_string(backend_id),
        status = escape_graphql_string(status),
    );
    access.execute(&mutation).await?;
    Ok(())
}

/// The `update_AgentBehavior` input body for binding a backend (and optionally
/// overriding the model). Both values are GraphQL-escaped. `model` is set only
/// when a newly-configured/detected backend serves a different model than the
/// behavior currently names — the runtime sends `behavior.model_name` to the
/// provider, so binding the backend without the model would request a model the
/// endpoint does not serve.
fn behavior_bind_input(backend_id: &str, model: Option<&str>) -> String {
    let mut input = format!(r#"backend_id: "{}""#, escape_graphql_string(backend_id));
    if let Some(model) = model {
        input.push_str(&format!(
            r#", model_name: "{}""#,
            escape_graphql_string(model)
        ));
    }
    input
}

/// Point the agent's default behavior at `backend_id` (and `model` when given).
/// Errors if the behavior does not exist (the home was never initialized past
/// identity).
async fn bind_default_behavior(
    access: &ConfigAccess,
    behavior_id: &str,
    backend_id: &str,
    model: Option<&str>,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior}" }} }},
                input: {{ {input} }}
            ) {{ _docID }}
        }}"#,
        behavior = escape_graphql_string(behavior_id),
        input = behavior_bind_input(backend_id, model),
    );
    let response = access.execute(&mutation).await?;
    let updated = response
        .get("data")
        .and_then(|data| data.get("update_AgentBehavior"))
        .and_then(Value::as_array)
        .map(|rows| !rows.is_empty())
        .unwrap_or(false);
    if !updated {
        anyhow::bail!(
            "default behavior {behavior_id} not found; run `defra-agent init` before onboarding"
        );
    }
    Ok(())
}

/// Prompt the operator to pick one of several stored backends.
async fn prompt_select_backend(reader: &mut StdinLines, backend_ids: &[String]) -> Result<String> {
    eprintln!("\nMultiple inference backends are configured. Which should the agent use?");
    for (index, id) in backend_ids.iter().enumerate() {
        eprintln!("  {}) {id}", index + 1);
    }
    let choice = prompt_line(reader, "> ").await?;
    let choice = choice.trim();
    let selection = choice
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1 && *n <= backend_ids.len());
    match selection {
        Some(n) => Ok(backend_ids[n - 1].clone()),
        None => anyhow::bail!("unrecognized choice: {choice:?}"),
    }
}

/// No usable backend and none detected: offer to launch a local server (print
/// instructions and re-run) or configure a remote one now. Returns the remote
/// backend config + model when the operator configures one, or `None` when they
/// choose to launch a local server.
async fn offer_launch_or_remote(
    reader: &mut StdinLines,
) -> Result<Option<(ResolvedBackendConfig, String)>> {
    eprintln!("\nNo usable inference backend is configured and no local server was detected.");
    eprintln!("  1) start a local server   (e.g. ollama / llama-server, then re-run `onboard`)");
    eprintln!("  2) configure a remote backend now");
    let choice = prompt_line(reader, "> ").await?;
    match choice.trim() {
        "1" | "" => {
            eprintln!("\nStart a local OpenAI-compatible inference server, for example:");
            eprintln!("  ollama serve            # then: ollama pull <model>");
            eprintln!("  llama-server -m <model.gguf> --port 8080");
            eprintln!(
                "onboard probes http://127.0.0.1:{{8080,11434,8000}}/v1 — once one is up, re-run `defra-agent onboard`."
            );
            Ok(None)
        }
        "2" => {
            let url = prompt_line(reader, "Backend base URL (incl. /v1): ").await?;
            let url = url.trim();
            if url.is_empty() {
                anyhow::bail!("no URL entered");
            }
            let model = prompt_line(reader, "Model name: ").await?;
            // A bare URL has no preset default; fall back to the init default,
            // matching `init`'s `--inference-url` handling.
            let model = non_empty(&model)
                .unwrap_or(DEFAULT_INIT_MODEL_NAME)
                .to_string();
            let key = prompt_secret(reader, "API key (optional, hidden): ").await?;
            let resolved = resolve_backend_config_with_preset(
                None,
                Some(url),
                None,
                None,
                non_empty(&key),
                None,
                BackendResolutionMode::Init,
            )?;
            Ok(Some((resolved, model)))
        }
        other => anyhow::bail!("unrecognized choice: {other:?}"),
    }
}

fn report(backend_id: &str, action: &str, behavior_id: &str) -> Result<()> {
    print_json(&json!({
        "status": "ok",
        "action": action,
        "backend_id": backend_id,
        "bound_behavior_id": behavior_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_input_sets_only_backend_when_no_model() {
        assert_eq!(
            behavior_bind_input("did:key:z:backend", None),
            r#"backend_id: "did:key:z:backend""#
        );
    }

    #[test]
    fn bind_input_sets_model_when_given() {
        let input = behavior_bind_input("did:key:z:backend", Some("llama3"));
        assert!(
            input.contains(r#"backend_id: "did:key:z:backend""#),
            "{input}"
        );
        assert!(input.contains(r#"model_name: "llama3""#), "{input}");
    }

    #[test]
    fn bind_input_escapes_injection_in_both_fields() {
        // A quote in either value must be escaped, never break out of the string.
        let input = behavior_bind_input(r#"b"x"#, Some(r#"m"y"#));
        assert!(input.contains(r#"backend_id: "b\"x""#), "{input}");
        assert!(input.contains(r#"model_name: "m\"y""#), "{input}");
        // No unescaped quote sequence that would close the field early.
        assert!(!input.contains(r#"b"x"#), "{input}");
    }

    #[test]
    fn model_explicit_wins_and_is_trimmed() {
        assert_eq!(
            resolve_onboard_model(Some(BackendPresetArg::Ollama), Some("  my-model  ")).unwrap(),
            "my-model"
        );
    }

    #[test]
    fn model_empty_is_rejected() {
        assert!(resolve_onboard_model(Some(BackendPresetArg::Ollama), Some("   ")).is_err());
        assert!(validated_model_override(Some("  ")).is_err());
        assert_eq!(
            validated_model_override(Some(" x ")).unwrap(),
            Some("x".to_string())
        );
        assert_eq!(validated_model_override(None).unwrap(), None);
    }

    #[test]
    fn model_uses_preset_default_when_available() {
        // Ollama has a safe default; onboard must not substitute an OpenAI model.
        let ollama = resolve_onboard_model(Some(BackendPresetArg::Ollama), None).unwrap();
        assert_eq!(ollama, crate::DEFAULT_OLLAMA_MODEL_NAME);
    }

    #[test]
    fn model_errors_for_preset_without_a_safe_default() {
        // OpenRouter/vLLM/OpenAI/generic have no safe default — require --model.
        for preset in [
            BackendPresetArg::OpenRouter,
            BackendPresetArg::Vllm,
            BackendPresetArg::OpenAi,
        ] {
            assert!(
                resolve_onboard_model(Some(preset), None).is_err(),
                "{preset:?} must require an explicit model"
            );
        }
    }

    #[test]
    fn model_defaults_to_init_default_for_bare_url() {
        // No preset (a bare --inference-url) mirrors init's URL default.
        assert_eq!(
            resolve_onboard_model(None, None).unwrap(),
            DEFAULT_INIT_MODEL_NAME
        );
    }
}
