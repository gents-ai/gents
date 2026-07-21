//! First-run setup and resume: locate the persistent demo home, read the saved
//! agent DID, initialize a curated agent (read-only tools, `defra_query`
//! scoped to the agent-config preset),
//! and seed the demo skills.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use super::backend::BackendChoice;
use super::util::{path_arg, run_cli_json, run_cli_text};

pub(super) fn resolve_home(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".gents-demo")
    })
}

pub(super) fn read_agent_did(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join("init.json")).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    json.get("agent_did")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Initialize a curated demo agent in `home` and return its DID. The caller
/// (re)creates the tool root afterwards — `--dangerously-overwrite` wipes it.
pub(super) async fn init_agent(
    bin: &Path,
    home: &Path,
    work: &Path,
    backend: &BackendChoice,
    agent_name: &str,
) -> Result<String> {
    let mut init_args: Vec<String> = vec![
        "init".into(),
        "--home".into(),
        path_arg(home),
        "--dangerously-overwrite".into(),
        "--agent-name".into(),
        agent_name.into(),
        "--tool-root".into(),
        path_arg(work),
        // The demo agent is a learning environment: it can read (never
        // write) its own configuration surface, so "how am I configured?"
        // and "why doesn't X work?" are answerable in chat.
        "--enable-defra-query".into(),
        "--defra-query-collection".into(),
        "agent-config".into(),
    ];
    init_args.extend(backend.init_args.iter().cloned());
    let init = run_cli_json(bin, &init_args).await?;
    init.get("agent_did")
        .and_then(Value::as_str)
        .context("init did not return agent_did")
        .map(ToString::to_string)
}

pub(super) async fn seed_demo_skills(bin: &Path, graphql: &str, agent_did: &str) {
    let skills = [
        (
            "summarize",
            "Summarize",
            "Distill long text into a concise summary.",
            "When asked to summarize: capture the key points and decisions, drop filler, \
             and keep it short and faithful to the source.",
        ),
        (
            "fleet-guide",
            "Fleet Guide",
            "Explain this gents demo and suggest what to try next.",
            "You are running inside `gents demo`, an agent whose state lives in a DefraDB \
             control plane. Explain P2P pairing and cross-node subagent delegation in plain \
             terms, and suggest the user try `pair` then `delegate` in the demo shell. You can \
             read your own configuration with the defra_query tool (behaviors, tool selections, \
             backends, schedules, pairings) — use it to answer questions like \"how am I \
             configured?\" or to diagnose why something is not working, and to teach the user \
             what each piece does.",
        ),
    ];
    for (id, name, description, instructions) in skills {
        let args = [
            "config",
            "skill",
            "add",
            "--graphql",
            graphql,
            "--agent-did",
            agent_did,
            "--skill-id",
            id,
            "--name",
            name,
            "--scope",
            "principal",
            "--description",
            description,
            "--instructions",
            instructions,
        ];
        if let Err(error) = run_cli_text(bin, &args.map(String::from)).await {
            eprintln!("  (skill {id} not seeded: {error})");
        }
    }
}
