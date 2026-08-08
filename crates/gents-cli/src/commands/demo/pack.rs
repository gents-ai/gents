//! Non-interactive pack runs: `gents demo run <pack>`.
//!
//! A pack is a self-contained desired-state root (its own `schemas/` plus the
//! config documents) with an `experiment.json` describing how to drive it. The
//! interactive demo shell is the same machinery with a human at the wheel;
//! this is the version CI can gate on, so it returns a non-zero exit code and
//! writes its artifacts under `<pack>/runs/<job_id>/`.
//!
//! The hand-run sequence this replaces is eight steps with two traps: the pack
//! applies *after* the runtime is ready, so its backend is unprobed for up to
//! one probe interval and the behaviors read as unavailable while the server
//! reports `serving`; and seeding before the event source observes the seed
//! collection is a silent no-op, because triggers are created/first-seen only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::fleet::{spawn_server_with_args, wait_http, wait_runtime_ready};
use super::util::{path_arg, run_cli_json};
use crate::cli::args::DemoRunArgs;
use crate::desired_state::interpolate::interpolate;
use crate::graphql_access::post_graphql;
use gents::graphql::escape_graphql_string;

#[derive(Debug, Deserialize)]
struct PackManifest {
    name: String,
    #[serde(default)]
    description: String,
    init: PackInit,
    seed: PackSeed,
    #[serde(default)]
    default_prompt: String,
    expect: PackExpect,
    #[serde(default = "default_timeout")]
    await_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    240
}

#[derive(Debug, Deserialize)]
struct PackInit {
    inference_url: String,
    model_name: String,
    #[serde(default)]
    backend_preset: Option<String>,
    #[serde(default)]
    openai_wire_api: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackSeed {
    collection: String,
    job_id_field: String,
    prompt_field: String,
    #[serde(default)]
    fields: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct PackExpect {
    trigger_ids: Vec<String>,
    #[serde(default)]
    collection_counts: BTreeMap<String, u64>,
}

/// Resolve a pack by path, or by name under `experiments/`.
fn resolve_pack(target: &str) -> Result<PathBuf> {
    let direct = PathBuf::from(target);
    if direct.join("experiment.json").is_file() {
        return Ok(direct);
    }
    let under_experiments = PathBuf::from("experiments").join(target);
    if under_experiments.join("experiment.json").is_file() {
        return Ok(under_experiments);
    }
    bail!(
        "no pack at {} or {} (a pack is a directory containing experiment.json)",
        direct.display(),
        under_experiments.display()
    )
}

fn load_manifest(pack: &Path) -> Result<PackManifest> {
    let path = pack.join("experiment.json");
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let expanded = interpolate(&raw).map_err(|missing| {
        anyhow::anyhow!(
            "{} references unset environment variable(s): {}",
            path.display(),
            missing.join(", ")
        )
    })?;
    serde_json::from_str(&expanded).with_context(|| format!("parsing {}", path.display()))
}

pub(crate) async fn list(root: &Path) -> Result<()> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let entries =
        std::fs::read_dir(root).with_context(|| format!("reading pack root {}", root.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join("experiment.json").is_file() {
            continue;
        }
        match load_manifest(&path) {
            Ok(manifest) => rows.push((manifest.name, manifest.description)),
            Err(error) => rows.push((
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                format!("(unreadable: {error})"),
            )),
        }
    }
    rows.sort();
    if rows.is_empty() {
        println!("no packs under {}", root.display());
        return Ok(());
    }
    for (name, description) in rows {
        println!("{name:<20} {description}");
    }
    Ok(())
}

/// Wait for the trigger engine to announce it is watching `collection`.
///
/// This is the real go-signal, and it is strictly later than "serving": the
/// event source only starts observing once the behaviors behind the triggers
/// are runnable, which needs the pack's backend probed. Seeding earlier is
/// silently dropped rather than rejected.
async fn wait_for_event_source(log: &Path, collection: &str, deadline: Duration) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if text
                .lines()
                .any(|line| observes_collection(&strip_ansi(line), collection))
            {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!(
        "timed out after {}s waiting for the event source to observe {collection}. \
         The pack's backend may still be unprobed — check {} for \
         'behavior unavailable after runtime reconcile'.",
        deadline.as_secs(),
        log.display()
    )
}

/// The runtime writes coloured tracing output even to a file, which splits
/// `source_collection=Name` with escape sequences. Strip them before matching.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // CSI: ESC '[' params… final-byte. The '[' introducer is itself inside
        // the final-byte range, so it must be consumed before scanning.
        if chars.peek() == Some(&'[') {
            chars.next();
        }
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
    out
}

/// Match the observe line for exactly `collection`, so `ExperimentJob` does
/// not satisfy a wait on a collection whose name it prefixes.
fn observes_collection(line: &str, collection: &str) -> bool {
    if !line.contains("event source now observing") {
        return false;
    }
    let needle = format!("source_collection={collection}");
    let Some(idx) = line.find(&needle) else {
        return false;
    };
    line[idx + needle.len()..]
        .chars()
        .next()
        .is_none_or(|next| !next.is_alphanumeric() && next != '_')
}

fn seed_mutation(seed: &PackSeed, job_id: &str, prompt: &str) -> String {
    let mut fields = vec![
        format!(
            "{}: \"{}\"",
            seed.job_id_field,
            escape_graphql_string(job_id)
        ),
        format!(
            "{}: \"{}\"",
            seed.prompt_field,
            escape_graphql_string(prompt)
        ),
    ];
    for (key, value) in &seed.fields {
        fields.push(format!("{key}: \"{}\"", escape_graphql_string(value)));
    }
    format!(
        "mutation {{ create_{}(input: {{ {} }}) {{ _docID }} }}",
        seed.collection,
        fields.join(", ")
    )
}

#[derive(Debug, Clone)]
struct StageResult {
    trigger_id: String,
    request_id: String,
    lifecycle_state: String,
}

async fn await_stages(
    graphql: &str,
    trigger_ids: &[String],
    deadline: Duration,
) -> Result<Vec<StageResult>> {
    let started = Instant::now();
    loop {
        let mut done: Vec<StageResult> = Vec::new();
        for trigger_id in trigger_ids {
            let query = format!(
                r#"{{ AgentRequest(filter: {{ caused_by_trigger_id: {{ _eq: "{}" }} }}) {{ request_id lifecycle_state }} }}"#,
                escape_graphql_string(trigger_id)
            );
            let Ok(resp) = post_graphql(graphql, &query).await else {
                continue;
            };
            let Some(rows) = resp.pointer("/data/AgentRequest").and_then(Value::as_array) else {
                continue;
            };
            for row in rows {
                let state = row
                    .get("lifecycle_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if matches!(state, "completed" | "failed" | "cancelled") {
                    done.push(StageResult {
                        trigger_id: trigger_id.clone(),
                        request_id: row
                            .get("request_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        lifecycle_state: state.to_string(),
                    });
                    break;
                }
            }
        }
        if done.len() == trigger_ids.len() {
            return Ok(done);
        }
        if started.elapsed() >= deadline {
            let seen: Vec<&str> = done.iter().map(|s| s.trigger_id.as_str()).collect();
            bail!(
                "timed out after {}s: reached a terminal state for [{}], expected [{}]",
                deadline.as_secs(),
                seen.join(", "),
                trigger_ids.join(", ")
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn count_rows(graphql: &str, collection: &str) -> u64 {
    let query = format!("{{ {collection} {{ _docID }} }}");
    post_graphql(graphql, &query)
        .await
        .ok()
        .and_then(|resp| {
            resp.pointer(&format!("/data/{collection}"))
                .and_then(Value::as_array)
                .map(|rows| rows.len() as u64)
        })
        .unwrap_or(0)
}

async fn token_totals(graphql: &str) -> (u64, u64) {
    let query = "{ InferenceCall { prompt_tokens completion_tokens } }";
    let Ok(resp) = post_graphql(graphql, query).await else {
        return (0, 0);
    };
    let Some(rows) = resp
        .pointer("/data/InferenceCall")
        .and_then(Value::as_array)
    else {
        return (0, 0);
    };
    rows.iter().fold((0, 0), |(p, c), row| {
        (
            p + row
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            c + row
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
    })
}

pub(crate) async fn run(args: DemoRunArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the gents binary path")?;
    let pack = resolve_pack(&args.pack)?;
    let manifest = load_manifest(&pack)?;
    let job_id = args
        .job_id
        .clone()
        .unwrap_or_else(|| format!("exp-{}", std::process::id()));
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| manifest.default_prompt.clone());
    if prompt.trim().is_empty() {
        bail!("no prompt: pass --prompt or give the pack a default_prompt");
    }

    // A fresh home per run by default. Triggers are first-seen, so a reused
    // home can silently skip a stage whose source rows already existed.
    let temp_home = args.home.is_none();
    let home = args.home.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("gents-pack-{}-{job_id}", manifest.name))
    });
    if temp_home && home.exists() {
        std::fs::remove_dir_all(&home).ok();
    }
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating pack home {}", home.display()))?;

    println!("pack     {} ({})", manifest.name, pack.display());
    println!("job_id   {job_id}");
    println!("home     {}", home.display());
    println!("endpoint {}", manifest.init.inference_url);
    println!("model    {}", manifest.init.model_name);

    let mut init_args: Vec<String> = vec![
        "init".into(),
        "--home".into(),
        path_arg(&home),
        "--dangerously-overwrite".into(),
        "--inference-url".into(),
        manifest.init.inference_url.clone(),
        "--model-name".into(),
        manifest.init.model_name.clone(),
        "--tool-package".into(),
        "minimal".into(),
    ];
    if let Some(preset) = manifest.init.backend_preset.as_deref() {
        init_args.push("--backend-preset".into());
        init_args.push(preset.into());
    }
    if let Some(wire) = manifest.init.openai_wire_api.as_deref() {
        init_args.push("--openai-wire-api".into());
        init_args.push(wire.into());
    }
    let init = run_cli_json(&bin, &init_args).await?;
    let agent_did = init
        .get("agent_did")
        .and_then(Value::as_str)
        .context("init did not return agent_did")?
        .to_string();

    let port = args.http_port;
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let log = home.join("server.log");
    let started = Instant::now();

    let mut server = spawn_server_with_pack(&bin, &home, port, &log, &pack)?;
    let outcome = async {
        wait_http(&format!("http://127.0.0.1:{port}/healthz"), &mut server).await?;
        wait_runtime_ready(&graphql, &agent_did, &mut server).await?;
        println!(
            "runtime  ready; waiting for the event source to observe {}…",
            manifest.seed.collection
        );
        wait_for_event_source(
            &log,
            &manifest.seed.collection,
            Duration::from_secs(manifest.await_timeout_secs),
        )
        .await?;

        let mutation = seed_mutation(&manifest.seed, &job_id, &prompt);
        post_graphql(&graphql, &mutation)
            .await
            .context("seeding the pack")?;
        println!("seeded   1 {} document", manifest.seed.collection);

        let stages = await_stages(
            &graphql,
            &manifest.expect.trigger_ids,
            Duration::from_secs(manifest.await_timeout_secs),
        )
        .await?;

        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for collection in manifest.expect.collection_counts.keys() {
            counts.insert(collection.clone(), count_rows(&graphql, collection).await);
        }
        let (prompt_tokens, completion_tokens) = token_totals(&graphql).await;
        Ok::<_, anyhow::Error>((stages, counts, prompt_tokens, completion_tokens))
    }
    .await;

    let _ = server.start_kill();

    let (stages, counts, prompt_tokens, completion_tokens) = match outcome {
        Ok(values) => values,
        Err(error) => {
            eprintln!("\nrun failed: {error:#}");
            eprintln!("server log: {}", log.display());
            return Err(error);
        }
    };

    let elapsed = started.elapsed();
    let mut failures: Vec<String> = Vec::new();
    for stage in &stages {
        if stage.lifecycle_state != "completed" {
            failures.push(format!(
                "{} ended {}",
                stage.trigger_id, stage.lifecycle_state
            ));
        }
    }
    for (collection, expected) in &manifest.expect.collection_counts {
        let actual = counts.get(collection).copied().unwrap_or(0);
        if actual < *expected {
            failures.push(format!(
                "{collection}: expected at least {expected}, found {actual}"
            ));
        }
    }

    let run_dir = pack.join("runs").join(&job_id);
    std::fs::create_dir_all(&run_dir).ok();
    let meta = json!({
        "pack": manifest.name,
        "job_id": job_id,
        "agent_did": agent_did,
        "endpoint": manifest.init.inference_url,
        "model": manifest.init.model_name,
        "elapsed_secs": elapsed.as_secs(),
        "prompt": prompt,
        "stages": stages.iter().map(|s| json!({
            "trigger_id": s.trigger_id,
            "request_id": s.request_id,
            "lifecycle_state": s.lifecycle_state,
        })).collect::<Vec<_>>(),
        "collection_counts": counts,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "ok": failures.is_empty(),
        "failures": failures,
    });
    let meta_path = run_dir.join("meta.json");
    if let Ok(text) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(&meta_path, text);
    }

    println!();
    for stage in &stages {
        println!(
            "  {:<12} {:<10} {}",
            stage.trigger_id, stage.lifecycle_state, stage.request_id
        );
    }
    for (collection, actual) in &counts {
        println!("  {collection:<12} {actual} document(s)");
    }
    println!(
        "  tokens       {prompt_tokens} prompt + {completion_tokens} completion in {}s",
        elapsed.as_secs()
    );
    println!("  artifacts    {}", meta_path.display());
    if temp_home && !args.keep_home {
        std::fs::remove_dir_all(&home).ok();
    } else {
        println!("  home         {}", home.display());
    }

    if failures.is_empty() {
        println!("\nok");
        Ok(())
    } else {
        bail!(
            "pack run did not meet expectations: {}",
            failures.join("; ")
        )
    }
}

fn spawn_server_with_pack(
    bin: &Path,
    home: &Path,
    port: u16,
    log: &Path,
    pack: &Path,
) -> Result<tokio::process::Child> {
    let root = path_arg(pack);
    spawn_server_with_args(bin, home, port, log, &["--apply-root", &root])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: tracing colours its file output, so the raw bytes are
    /// `source_collection` ESC `=` ESC `ExperimentJob`. Matching the plain
    /// substring silently never fired and the runner timed out with the
    /// observe line sitting in the log.
    const COLOURED: &str = "\u{1b}[32m INFO\u{1b}[0m \u{1b}[2mgents::trigger_engine::event_source\u{1b}[0m\u{1b}[2m:\u{1b}[0m event source now observing source collection \u{1b}[3msource_collection\u{1b}[0m\u{1b}[2m=\u{1b}[0mExperimentJob \u{1b}[3mgeneration\u{1b}[0m\u{1b}[2m=\u{1b}[0m3";

    #[test]
    fn matches_the_observe_line_through_ansi_colouring() {
        assert!(observes_collection(&strip_ansi(COLOURED), "ExperimentJob"));
    }

    #[test]
    fn does_not_match_a_different_collection() {
        assert!(!observes_collection(
            &strip_ansi(COLOURED),
            "ExperimentFinding"
        ));
    }

    #[test]
    fn does_not_match_a_name_that_merely_prefixes_another() {
        let line = "event source now observing source collection source_collection=ExperimentJobArchive generation=3";
        assert!(!observes_collection(line, "ExperimentJob"));
        assert!(observes_collection(line, "ExperimentJobArchive"));
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(!observes_collection(
            "gents behavior started behavior_id=exp-stage1",
            "ExperimentJob"
        ));
    }
}
