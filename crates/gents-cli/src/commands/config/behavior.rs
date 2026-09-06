use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::agent::persona_presets::{self, PresetFields};
use gents::graphql::escape_graphql_string;
use gents::{
    default_behavior_id_for_agent, AgentBehaviorDocument as AgentBehavior, AgentIdentity,
    Collection,
};
use gents_protocol::persona::{LocalPersonaRequestRecord, PERSONA_AUTHORITY_LOCAL_SELF};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};
use crate::request_helpers::resolve_dual_id;
use crate::{
    graphql_rows, load_initialized_home_identity, print_json, read_init_config,
    resolve_config_access, resolve_home_dir, EXPORT_AGENT_BEHAVIOR_FIELDS,
    EXPORT_INFERENCE_PROFILE_FIELDS, EXPORT_TOOL_SELECTION_FIELDS,
};

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
    // This raw field edit deliberately stays outside persona admission (see
    // `gents::agent::persona_ops`). The shared writer owns effective-row
    // projection and reference validation for this sparse update.
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

const PERSONA_REQUEST_POLL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default, Deserialize)]
struct PersonaRequestStatusRow {
    status: Option<String>,
    status_detail: Option<String>,
    applied_behavior_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SourceBehaviorRow {
    agent_did: Option<String>,
    backend_id: Option<String>,
    model_name: Option<String>,
    inference_profile_id: Option<String>,
}

fn local_identity(home: Option<&std::path::Path>) -> Result<Arc<dyn AgentIdentity>> {
    let home_dir = resolve_home_dir(home);
    let config = read_init_config(&home_dir)?.with_context(|| {
        format!(
            "no init config found in {}; run `gents init` first",
            home_dir.display()
        )
    })?;
    load_initialized_home_identity(&home_dir, &config)
}

async fn poll_persona_request(access: &ConfigAccess, request_key: &str) -> Result<String> {
    let key = escape_graphql_string(request_key);
    let query = format!(
        r#"{{ PersonaConfigRequest(filter: {{ request_key: {{ _eq: "{key}" }} }}) {{ status status_detail applied_behavior_id }} }}"#
    );
    let deadline = tokio::time::Instant::now() + PERSONA_REQUEST_POLL_TIMEOUT;
    loop {
        if let Some(row) = graphql_rows(access, "PersonaConfigRequest", &query)
            .await?
            .into_iter()
            .next()
        {
            let row: PersonaRequestStatusRow = serde_json::from_value(row)?;
            match row.status.as_deref() {
                Some("applied") => {
                    return row
                        .applied_behavior_id
                        .context("applied persona request missing behavior id")
                }
                Some("rejected") => anyhow::bail!("{}", row.status_detail.unwrap_or_default()),
                _ => {}
            }
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "persona request {request_key} remained pending for {PERSONA_REQUEST_POLL_TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn submit_local_persona(
    graphql: &str,
    home: Option<&std::path::Path>,
    mut record: LocalPersonaRequestRecord,
) -> Result<String> {
    let identity = local_identity(home)?;
    anyhow::ensure!(
        identity.did() == record.agent_did,
        "initialized home signer {} does not own target agent {}",
        identity.did(),
        record.agent_did
    );
    record.local_signature = identity.sign(&record.signing_payload()).await?;
    record.validate_shape()?;
    let access = ConfigAccess::Graphql(graphql.to_string());
    access
        .execute(&gents::agent::persona_ops::local_persona_request_mutation(
            &record,
        ))
        .await?;
    poll_persona_request(&access, &record.request_key).await
}

fn local_record(
    agent_did: String,
    op: &str,
    behavior_id: Option<String>,
    clone_from: Option<String>,
    persona_name: Option<String>,
    backend_model: Option<String>,
    root: Option<String>,
    preset: Option<String>,
    profile_id: Option<String>,
) -> LocalPersonaRequestRecord {
    LocalPersonaRequestRecord {
        request_key: format!("cli-{}", uuid::Uuid::new_v4()),
        requester_did: agent_did.clone(),
        agent_did: agent_did.clone(),
        authority_kind: PERSONA_AUTHORITY_LOCAL_SELF.to_string(),
        local_signer_did: agent_did,
        op: op.to_string(),
        behavior_id,
        clone_from,
        persona_name,
        backend_model,
        root,
        preset,
        profile_id,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        local_signature: Vec::new(),
    }
}

fn resolve_backend_model(args: &BehaviorCreateArgs) -> Result<String> {
    match (
        args.model
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        args.backend_id
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
        args.model_name
            .as_deref()
            .filter(|value| !value.trim().is_empty()),
    ) {
        (Some(model), None, None) => Ok(model.trim().to_string()),
        (None, Some(backend), Some(model)) => Ok(format!("{}|{}", backend.trim(), model.trim())),
        _ => anyhow::bail!("provide --model or exactly --backend-id plus --model-name"),
    }
}

async fn source_behavior(graphql: &str, behavior_id: &str) -> Result<SourceBehaviorRow> {
    let access = ConfigAccess::Graphql(graphql.to_string());
    let id = escape_graphql_string(behavior_id);
    let query = format!(
        r#"{{ AgentBehavior(filter: {{ behavior_id: {{ _eq: "{id}" }} }}, limit: 1) {{ agent_did backend_id model_name inference_profile_id }} }}"#
    );
    let row = graphql_rows(&access, "AgentBehavior", &query)
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("unknown behavior_id {behavior_id:?}"))?;
    Ok(serde_json::from_value(row)?)
}

pub(super) async fn behavior_create(args: BehaviorCreateArgs) -> Result<()> {
    let model = resolve_backend_model(&args)?;
    let record = local_record(
        args.agent_did.clone(),
        "create",
        None,
        args.clone_from.clone(),
        Some(args.display_name.clone()),
        Some(model),
        args.root.clone(),
        args.preset.clone(),
        args.profile_id.clone(),
    );
    let request_key = record.request_key.clone();
    let behavior_id = submit_local_persona(&args.graphql, args.home.as_deref(), record).await?;
    print_json(&json!({"status":"applied", "request_key":request_key, "behavior_id":behavior_id}))
}

pub(super) async fn behavior_clone(args: BehaviorCloneArgs) -> Result<()> {
    let source = source_behavior(&args.graphql, &args.source_behavior_id).await?;
    let agent_did = source
        .agent_did
        .context("source behavior missing agent_did")?;
    let model = args.model.clone().unwrap_or_else(|| {
        format!(
            "{}|{}",
            source.backend_id.unwrap_or_default(),
            source.model_name.unwrap_or_default()
        )
    });
    let record = local_record(
        agent_did,
        "create",
        None,
        Some(args.source_behavior_id.clone()),
        Some(args.display_name.clone()),
        Some(model),
        args.root.clone(),
        None,
        args.profile_id.clone().or(source.inference_profile_id),
    );
    let request_key = record.request_key.clone();
    let behavior_id = submit_local_persona(&args.graphql, args.home.as_deref(), record).await?;
    print_json(&json!({"status":"applied", "request_key":request_key, "behavior_id":behavior_id}))
}

pub(super) async fn behavior_disable(args: BehaviorDisableArgs) -> Result<()> {
    let source = source_behavior(&args.graphql, &args.behavior_id).await?;
    let agent_did = source
        .agent_did
        .context("source behavior missing agent_did")?;
    let record = local_record(
        agent_did,
        "disable",
        Some(args.behavior_id.clone()),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let request_key = record.request_key.clone();
    let behavior_id = submit_local_persona(&args.graphql, args.home.as_deref(), record).await?;
    print_json(&json!({"status":"applied", "request_key":request_key, "behavior_id":behavior_id}))
}

// -- enriched show: base AgentBehavior fields plus a `resolved` section
// (the referenced ToolSelection's knobs, its preset classification, root,
// and the referenced InferenceProfile). Read-only, so this reads straight
// through `ConfigAccess`/`graphql_rows` rather than the persona-request
// channel above.

async fn load_document(
    access: &ConfigAccess,
    collection: Collection,
    fields: &str,
    id: &str,
) -> Result<Option<Value>> {
    let escaped_id = escape_graphql_string(id);
    let query = format!(
        r#"{{
            {collection_type}(filter: {{ {unique_field}: {{ _eq: "{escaped_id}" }} }}) {{
                {fields}
            }}
        }}"#,
        collection_type = collection.graphql_type(),
        unique_field = collection.unique_field(),
    );
    let rows = graphql_rows(access, collection.graphql_type(), &query).await?;
    Ok(rows.into_iter().next())
}

fn string_vec(value: &Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Classify a loaded `ToolSelection` row against the built-in permission
/// presets (`gents::agent::persona_presets`): the same discriminating fields
/// the directory projection classifies on. `None` (no exact match) means
/// "custom" in the printed output.
fn classify_preset(selection: &Value) -> Option<&'static str> {
    let fields = PresetFields {
        enable_file_tools: selection
            .get("enable_file_tools")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        file_tools_mode: selection
            .get("file_tools_mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        enable_bash: selection
            .get("enable_bash")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        bash_mode: selection
            .get("bash_mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        command_allowed_argv_prefixes: string_vec(selection, "command_allowed_argv_prefixes"),
        command_forbidden_argv_prefixes: string_vec(selection, "command_forbidden_argv_prefixes"),
        read_only_command_allowlist: string_vec(selection, "read_only_command_allowlist"),
        enable_self_config: selection
            .get("enable_self_config")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        write_tools: string_vec(selection, "write_tools"),
    };
    persona_presets::preset_name(&fields)
}

pub(super) async fn behavior_show(args: ConfigShowArgs) -> Result<()> {
    let id = resolve_dual_id(
        "behavior",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    args.output
        .ensure_supported("config behavior show", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for config behavior show")?;

    let mut row = load_document(
        &access,
        Collection::AgentBehavior,
        EXPORT_AGENT_BEHAVIOR_FIELDS,
        &id,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("not found: no AgentBehavior document with behavior_id {id}"))?;

    let tool_selection_id = row
        .get("tool_selection_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let profile_id = row
        .get("inference_profile_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let tool_selection = match tool_selection_id.as_deref() {
        Some(selection_id) => {
            load_document(
                &access,
                Collection::ToolSelection,
                EXPORT_TOOL_SELECTION_FIELDS,
                selection_id,
            )
            .await?
        }
        None => None,
    };
    let profile = match profile_id.as_deref() {
        Some(profile_id) => {
            load_document(
                &access,
                Collection::InferenceProfile,
                EXPORT_INFERENCE_PROFILE_FIELDS,
                profile_id,
            )
            .await?
        }
        None => None,
    };
    let preset_name = tool_selection
        .as_ref()
        .map(|selection| classify_preset(selection).unwrap_or("custom"));
    let root = tool_selection
        .as_ref()
        .and_then(|selection| selection.get("file_tool_root"))
        .cloned()
        .unwrap_or(Value::Null);

    let resolved = json!({
        "root": root,
        "preset_name": preset_name,
        "profile": profile,
        "tool_selection": tool_selection,
    });
    if let Value::Object(ref mut map) = row {
        map.insert("resolved".to_string(), resolved);
    }
    print_json(&row)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use defra_node::{EmbeddedNode, StorageBackend};
    use gents::agent::persona_ops::{
        apply_persona_request, PersonaCatalogView, PersonaOp, PersonaRequestDoc,
    };
    use gents::agent::persona_presets::PRESET_WRITE;
    use gents::config_client::{
        write_inference_backend_document, ConfigAccess as LocalConfigAccess,
        InferenceBackendUpsertDocument,
    };
    use gents::{
        ensure_runtime_schemas, load_tool_selection, upsert_inference_profile, BackendProviderKind,
        InferenceProfile, UNKNOWN_PROBE_STATUS,
    };

    use super::*;
    use crate::cli::ToolPackageArg;
    use crate::commands::init::tool_selection_for_package;

    /// Cross-crate init-parity drift guard (deferred from Task 2's review to
    /// this task, since `gents-cli` is the one crate where both authoritative
    /// sources are visible): `persona_ops`' preset-minted "write"
    /// `ToolSelection` — materialized here through the public
    /// `apply_persona_request` entry point, the same one the reconciler and
    /// this crate's `behavior create`/`clone` commands ultimately drive —
    /// must equal `commands::init::tool_selection_for_package(Write)`
    /// field-for-field outside `selection_id`/`display_name`/`file_tool_root`
    /// (see `gents::agent::persona_presets`' module doc for why those three
    /// are excluded). `gents::agent::persona_ops`'s own intra-crate test pins
    /// the same contract from the `gents` side by calling its private minting
    /// function directly.
    #[tokio::test]
    async fn persona_write_preset_matches_init_write_package_field_for_field() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = EmbeddedNode::builder()
            .data_path(tempdir.path().join("data"))
            // Pin the sole durable backend explicitly so this test cannot
            // silently drift if another backend is introduced later.
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await
            .expect("embedded node boots");
        ensure_runtime_schemas(&node)
            .await
            .expect("runtime schemas register");
        let node = Arc::new(node);

        write_inference_backend_document(
            &LocalConfigAccess::Local(node.clone()),
            &InferenceBackendUpsertDocument {
                backend_id: "openai".to_string(),
                name: "OpenAI".to_string(),
                provider_kind: BackendProviderKind::OpenAiCompatible,
                openai_wire_api: None,
                endpoint: "https://api.openai.com/v1".to_string(),
                api_key: None,
                api_key_env_var: None,
                max_concurrent: 1,
                max_queue_depth: 1,
                enabled: true,
                models_on_add: vec!["gpt-5".to_string()],
                models_on_update: None,
                probe_status: UNKNOWN_PROBE_STATUS.to_string(),
            },
        )
        .await?;
        upsert_inference_profile(
            &node,
            &InferenceProfile {
                profile_id: "profile-1".to_string(),
                ..Default::default()
            },
        )
        .await?;

        let doc = PersonaRequestDoc {
            request_key: "drift-guard-1".to_string(),
            agent_did: "did:key:drift-guard".to_string(),
            op_raw: "create".to_string(),
            op: Some(PersonaOp::Create { clone_from: None }),
            persona_name: Some("Drift Guard".to_string()),
            backend_model: Some("openai|gpt-5".to_string()),
            root: Some("".to_string()),
            preset: Some(PRESET_WRITE.to_string()),
            profile_id: Some("profile-1".to_string()),
            ..Default::default()
        };
        let outcome = apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;
        assert!(!outcome.repaired);

        // Compare what each channel actually PERSISTS, not init's in-memory
        // struct: the shared write path (`write_tool_selection_document`)
        // never emits `[]` for an empty list field (typed as `JsonArray`,
        // corrupts nillable array columns — see AGENTS.md), so an
        // in-memory `Some(vec![])` and a round-tripped `None` are the same
        // persisted value. Writing `init_minted` through the identical path
        // before comparing keeps the assertion honest about that.
        let init_minted = tool_selection_for_package(
            &doc.agent_did,
            "sel-drift-guard-1-init",
            ToolPackageArg::Write,
            false,
            false,
            Vec::new(),
        );
        gents::config_client::write_tool_selection_document(
            &gents::config_client::ConfigAccess::Local(node.clone()),
            &init_minted,
        )
        .await?;

        let mut persona_minted = load_tool_selection(&node, "sel-drift-guard-1")
            .await?
            .expect("persona-minted selection exists");
        let mut init_minted = load_tool_selection(&node, "sel-drift-guard-1-init")
            .await?
            .expect("init-minted selection exists");

        // Fields the two channels are explicitly allowed to differ on: id/
        // display_name are never varied by init, and root is a dimension the
        // persona layer treats separately from the preset.
        persona_minted.selection_id = "same".to_string();
        init_minted.selection_id = "same".to_string();
        persona_minted.display_name = None;
        init_minted.display_name = None;
        persona_minted.file_tool_root = None;
        init_minted.file_tool_root = None;

        assert_eq!(persona_minted, init_minted);
        Ok(())
    }
}
