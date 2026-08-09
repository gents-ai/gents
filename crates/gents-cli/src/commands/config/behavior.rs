use std::time::Duration;

use anyhow::{Context, Result};
use gents::agent::persona_presets::{self, PresetFields};
use gents::graphql::escape_graphql_string;
use gents::{default_behavior_id_for_agent, AgentBehaviorDocument as AgentBehavior, Collection};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};
use crate::request_helpers::resolve_dual_id;
use crate::{
    authenticated_default_graphql_access, graphql_rows, print_json, resolve_config_access,
    EXPORT_AGENT_BEHAVIOR_FIELDS, EXPORT_INFERENCE_PROFILE_FIELDS, EXPORT_TOOL_SELECTION_FIELDS,
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
    let access = authenticated_default_graphql_access(&args.graphql).await?;
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

// -- create / clone / disable: routed through the shared persona
// materializer (`gents::agent::persona_ops`), never re-implemented here.
//
// The CLI has no `Arc<EmbeddedNode>` to call `decide_persona_request` /
// `apply_persona_request` directly with when it is pointed at a running
// server (the normal case — `--graphql` addresses that server over HTTP, and
// `gents server` already runs the persona-request reconciler as a background
// task; see `crate::agent::p2p_reconcile::persona_requests`). So these
// commands submit a `PersonaConfigRequest` row — the exact channel the
// reconciler and the agent's own `configure_persona` self-config tool use —
// and poll it to a terminal status, mirroring
// `gents::self_config::persona_mutate`/`poll_persona_request`. This means
// admission and materialization happen entirely inside the reconciler's call
// to the shared core; nothing here duplicates that logic.

// A little more generous than `gents::self_config`'s in-process 5s: the CLI
// polls over HTTP GraphQL rather than sharing the node with the reconciler,
// so each poll pays a network round trip on top of the sweep itself.
const PERSONA_REQUEST_POLL_TIMEOUT: Duration = Duration::from_secs(10);
const PERSONA_REQUEST_POLL_INTERVAL: Duration = Duration::from_millis(200);

fn nullable_graphql_string(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => format!(r#""{}""#, escape_graphql_string(value)),
        None => "null".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn create_persona_request_mutation(
    request_key: &str,
    requester_did: &str,
    agent_did: &str,
    op: &str,
    behavior_id: Option<&str>,
    clone_from: Option<&str>,
    persona_name: Option<&str>,
    backend_model: Option<&str>,
    root: Option<&str>,
    preset: Option<&str>,
    profile_id: Option<&str>,
    now: &str,
) -> String {
    format!(
        r#"mutation {{
            create_PersonaConfigRequest(input: {{
                request_key: "{request_key}",
                requester_did: "{requester_did}",
                agent_did: "{agent_did}",
                op: "{op}",
                behavior_id: {behavior_id},
                clone_from: {clone_from},
                persona_name: {persona_name},
                backend_model: {backend_model},
                root: {root},
                preset: {preset},
                profile_id: {profile_id},
                created_at: "{now}",
                status: "pending"
            }}) {{ _docID }}
        }}"#,
        request_key = escape_graphql_string(request_key),
        requester_did = escape_graphql_string(requester_did),
        agent_did = escape_graphql_string(agent_did),
        op = escape_graphql_string(op),
        behavior_id = nullable_graphql_string(behavior_id),
        clone_from = nullable_graphql_string(clone_from),
        persona_name = nullable_graphql_string(persona_name),
        backend_model = nullable_graphql_string(backend_model),
        root = nullable_graphql_string(root),
        preset = nullable_graphql_string(preset),
        profile_id = nullable_graphql_string(profile_id),
        now = escape_graphql_string(now),
    )
}

#[derive(Debug, Default, Deserialize)]
struct PersonaRequestStatusRow {
    status: Option<String>,
    #[serde(default)]
    status_detail: Option<String>,
    #[serde(default)]
    applied_behavior_id: Option<String>,
}

/// Poll a freshly-submitted `PersonaConfigRequest` row until the runtime's
/// persona reconciler (subscribed to every `Update` event) drives it to a
/// terminal status, or [`PERSONA_REQUEST_POLL_TIMEOUT`] elapses. Mirrors
/// `gents::self_config::poll_persona_request`'s cadence.
async fn poll_persona_config_request(
    access: &ConfigAccess,
    request_key: &str,
) -> Result<PersonaRequestStatusRow> {
    let escaped = escape_graphql_string(request_key);
    let query = format!(
        r#"{{
            PersonaConfigRequest(filter: {{ request_key: {{ _eq: "{escaped}" }} }}) {{
                status
                status_detail
                applied_behavior_id
            }}
        }}"#
    );
    let deadline = tokio::time::Instant::now() + PERSONA_REQUEST_POLL_TIMEOUT;
    loop {
        let rows = graphql_rows(access, "PersonaConfigRequest", &query).await?;
        if let Some(row) = rows.into_iter().next() {
            let parsed: PersonaRequestStatusRow =
                serde_json::from_value(row).context("decode PersonaConfigRequest row")?;
            if parsed.status.as_deref() != Some("pending") {
                return Ok(parsed);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "persona request {request_key} is still pending after {PERSONA_REQUEST_POLL_TIMEOUT:?}; \
                 the runtime reconciler may need another moment — retry shortly, or check \
                 `gents config behavior list`"
            );
        }
        tokio::time::sleep(PERSONA_REQUEST_POLL_INTERVAL).await;
    }
}

/// Submit `mutation`, then poll `request_key` to a terminal status. On
/// `rejected`, the rejection detail (the exact `decide_persona_request`
/// message) is surfaced verbatim as the returned error so it reaches stderr
/// unmodified.
async fn submit_and_await_persona_request(
    access: &ConfigAccess,
    request_key: &str,
    mutation: &str,
) -> Result<String> {
    access.execute(mutation).await?;
    let outcome = poll_persona_config_request(access, request_key).await?;
    match outcome.status.as_deref() {
        Some("applied") => outcome
            .applied_behavior_id
            .context("applied persona request missing applied_behavior_id"),
        Some("rejected") => {
            anyhow::bail!("{}", outcome.status_detail.unwrap_or_default())
        }
        other => anyhow::bail!("persona request {request_key} in unexpected status {other:?}"),
    }
}

fn resolve_backend_model(
    model: Option<&str>,
    backend_id: Option<&str>,
    model_name: Option<&str>,
) -> Result<String> {
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let backend_id = backend_id.map(str::trim).filter(|value| !value.is_empty());
    let model_name = model_name.map(str::trim).filter(|value| !value.is_empty());
    match (model, backend_id, model_name) {
        (Some(model), None, None) => Ok(model.to_string()),
        (Some(_), _, _) => {
            anyhow::bail!("--model is mutually exclusive with --backend-id/--model-name")
        }
        (None, Some(backend_id), Some(model_name)) => Ok(format!("{backend_id}|{model_name}")),
        (None, _, _) => {
            anyhow::bail!(
                r#"provide --model "backend_id|model_name" or both --backend-id and --model-name"#
            )
        }
    }
}

pub(super) async fn behavior_create(args: BehaviorCreateArgs) -> Result<()> {
    let backend_model = resolve_backend_model(
        args.model.as_deref(),
        args.backend_id.as_deref(),
        args.model_name.as_deref(),
    )?;
    let access = authenticated_default_graphql_access(&args.graphql).await?;
    let request_key = format!("cli-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = create_persona_request_mutation(
        &request_key,
        &args.agent_did,
        &args.agent_did,
        "create",
        None,
        args.clone_from.as_deref(),
        Some(&args.display_name),
        Some(&backend_model),
        args.root.as_deref(),
        args.preset.as_deref(),
        args.profile_id.as_deref(),
        &now,
    );
    let behavior_id = submit_and_await_persona_request(&access, &request_key, &mutation).await?;
    print_json(&json!({
        "request_key": request_key,
        "status": "applied",
        "agent_did": args.agent_did,
        "persona_name": args.display_name,
        "behavior_id": behavior_id,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct SourceBehaviorRow {
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    inference_profile_id: Option<String>,
}

async fn load_source_behavior(
    access: &ConfigAccess,
    behavior_id: &str,
) -> Result<Option<SourceBehaviorRow>> {
    let escaped = escape_graphql_string(behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(filter: {{ behavior_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                agent_did
                backend_id
                model_name
                inference_profile_id
            }}
        }}"#
    );
    let rows = graphql_rows(access, "AgentBehavior", &query).await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_value(row).context("decode AgentBehavior row")?,
    ))
}

pub(super) async fn behavior_clone(args: BehaviorCloneArgs) -> Result<()> {
    let access = authenticated_default_graphql_access(&args.graphql).await?;
    let source = load_source_behavior(&access, &args.source_behavior_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown clone_from {:?} — no such AgentBehavior",
                args.source_behavior_id
            )
        })?;
    let agent_did = source.agent_did.unwrap_or_default();

    let backend_model = match args.model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => model.to_string(),
        _ => format!(
            "{}|{}",
            source.backend_id.unwrap_or_default(),
            source.model_name.unwrap_or_default()
        ),
    };
    let profile_id = args
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or(source.inference_profile_id);

    let request_key = format!("cli-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = create_persona_request_mutation(
        &request_key,
        &agent_did,
        &agent_did,
        "create",
        None,
        Some(&args.source_behavior_id),
        Some(&args.display_name),
        Some(&backend_model),
        args.root.as_deref(),
        None,
        profile_id.as_deref(),
        &now,
    );
    let behavior_id = submit_and_await_persona_request(&access, &request_key, &mutation).await?;
    print_json(&json!({
        "request_key": request_key,
        "status": "applied",
        "agent_did": agent_did,
        "clone_from": args.source_behavior_id,
        "persona_name": args.display_name,
        "behavior_id": behavior_id,
    }))
}

pub(super) async fn behavior_disable(args: BehaviorDisableArgs) -> Result<()> {
    let access = authenticated_default_graphql_access(&args.graphql).await?;
    let source = load_source_behavior(&access, &args.behavior_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown behavior_id {:?} — no such AgentBehavior",
                args.behavior_id
            )
        })?;
    let agent_did = source.agent_did.unwrap_or_default();

    let request_key = format!("cli-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mutation = create_persona_request_mutation(
        &request_key,
        &agent_did,
        &agent_did,
        "disable",
        Some(&args.behavior_id),
        None,
        None,
        None,
        None,
        None,
        None,
        &now,
    );
    let behavior_id = submit_and_await_persona_request(&access, &request_key, &mutation).await?;
    print_json(&json!({
        "request_key": request_key,
        "status": "applied",
        "agent_did": agent_did,
        "behavior_id": behavior_id,
    }))
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
    use gents::{ensure_runtime_schemas, load_tool_selection};

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
            // Explicit backend, matching this crate's convention
            // (`persistent_node_builder`): the builder's default is Redb,
            // whose defra-node feature is only transitively enabled — the CI
            // cli shard builds without it.
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await
            .expect("embedded node boots");
        ensure_runtime_schemas(&node)
            .await
            .expect("runtime schemas register");
        let node = Arc::new(node);

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
        // corrupts nillable array columns — see CLAUDE.md), so an
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
