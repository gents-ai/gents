use anyhow::{Context, Result};
use gents::{default_behavior_id_for_agent, AgentBehaviorDocument as AgentBehavior};
use serde_json::json;

use crate::cli::*;
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};
use crate::print_json;

pub(super) async fn behavior_set(args: BehaviorUpsertArgs) -> Result<()> {
    let behavior_id = args
        .behavior_id
        .clone()
        .unwrap_or_else(|| default_behavior_id_for_agent(&args.agent_did));
    let system_prompt = match args.system_prompt_file {
        Some(ref path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading system prompt from {}", path.display()))?,
        ),
        None => None,
    };
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let behavior = AgentBehavior {
        behavior_id: behavior_id.clone(),
        agent_did: args.agent_did.clone(),
        display_name: args.display_name.clone(),
        description: None,
        summary: None,
        system_prompt,
        request_context_template: None,
        backend_id: args.backend_id.clone(),
        model_name: args.model_name.clone(),
        tool_selection_id: args.tool_selection_id.clone(),
        inference_profile_id: args.inference_profile_id.clone(),
        compaction_strategy: args.compaction_strategy.clone(),
        compaction_threshold: args.compaction_threshold,
        enabled: args.enabled,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    let doc_id = write_agent_behavior_document(&access, &behavior).await?;
    let output = json!({
        "doc_id": doc_id,
        "behavior_id": behavior_id,
        "agent_did": args.agent_did,
        "backend_id": args.backend_id,
        "model_name": args.model_name,
        "tool_selection_id": args.tool_selection_id,
        "inference_profile_id": args.inference_profile_id,
        "enabled": args.enabled,
    });
    print_json(&output)?;
    Ok(())
}
