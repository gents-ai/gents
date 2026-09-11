use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::agent::persona_presets;
use gents::document_config::AgentBehavior;
use gents::graphql::escape_graphql_string;
use gents::{default_behavior_id_for_agent, AgentIdentity, Collection};
use gents_protocol::persona::{LocalPersonaRequestRecord, PERSONA_AUTHORITY_LOCAL_SELF};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};
use crate::request_helpers::resolve_dual_id;
use crate::{
    graphql_rows, load_initialized_home_identity, print_json, read_init_config,
    resolve_config_access, resolve_home_dir,
};

pub(super) async fn behavior_set(args: BehaviorUpsertArgs) -> Result<()> {
    let behavior_id = args
        .behavior_id
        .clone()
        .unwrap_or_else(|| default_behavior_id_for_agent(&args.agent_did));
    let access = ConfigAccess::Graphql(args.graphql.clone());
    // Raw set means one complete canonical document: omitted optionals clear,
    // no sparse legacy merge. `write_agent_behavior_document` validates
    // references (same-principal context/profile existence) inside its
    // transaction; this deliberately stays outside persona admission (see
    // `gents::agent::persona_ops`).
    let behavior = AgentBehavior {
        behavior_id: behavior_id.clone(),
        agent_did: args.agent_did.clone(),
        display_name: args.display_name.clone(),
        description: args.description.clone(),
        context_id: args.context_id.clone(),
        inference_profile_id: args.inference_profile_id.clone(),
        enabled: args.enabled,
        tags: args.tags.clone(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    let doc_id = write_agent_behavior_document(&access, &behavior).await?;
    let output = json!({
        "doc_id": doc_id,
        "behavior_id": behavior_id,
        "agent_did": args.agent_did,
        "context_id": args.context_id,
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

async fn poll_persona_request(
    access: &ConfigAccess,
    owner: &str,
    request_key: &str,
) -> Result<String> {
    let key = escape_graphql_string(request_key);
    let owner = escape_graphql_string(owner);
    let query = format!(
        r#"{{ PersonaConfigRequest(filter: {{ agent_did: {{ _eq: "{owner}" }}, request_key: {{ _eq: "{key}" }} }}, limit: 2) {{ status status_detail applied_behavior_id }} }}"#
    );
    let deadline = tokio::time::Instant::now() + PERSONA_REQUEST_POLL_TIMEOUT;
    loop {
        let mut rows = graphql_rows(access, "PersonaConfigRequest", &query).await?;
        anyhow::ensure!(rows.len() <= 1, "ambiguous persona request {request_key}");
        if let Some(row) = rows.pop() {
            let row: PersonaRequestStatusRow = serde_json::from_value(row)?;
            match row.status.as_deref() {
                Some("applied") => {
                    return row
                        .applied_behavior_id
                        .context("applied persona request missing behavior id");
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
    let mutation = gents::agent::persona_ops::local_persona_request_mutation(&record);
    access
        .write("cli.config.behavior.persona_request", &mutation)
        .await?;
    poll_persona_request(&access, &record.agent_did, &record.request_key).await
}

fn local_record(
    agent_did: String,
    op: &str,
    behavior_id: Option<String>,
    clone_from: Option<String>,
    persona_name: Option<String>,
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
        root,
        preset,
        profile_id,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        local_signature: Vec::new(),
    }
}

async fn require_source_behavior(graphql: &str, owner: &str, behavior_id: &str) -> Result<()> {
    let access = ConfigAccess::Graphql(graphql.to_owned());
    load_document(
        &access,
        Collection::AgentBehavior,
        "agent_did",
        Some(owner),
        behavior_id,
    )
    .await?
    .with_context(|| format!("unknown behavior_id {behavior_id:?} under {owner:?}"))?;
    Ok(())
}

pub(super) async fn behavior_create(args: BehaviorCreateArgs) -> Result<()> {
    let record = local_record(
        args.agent_did.clone(),
        "create",
        None,
        args.clone_from.clone(),
        Some(args.display_name.clone()),
        args.root.clone(),
        args.preset.clone(),
        Some(args.profile_id.clone()),
    );
    let request_key = record.request_key.clone();
    let behavior_id = submit_local_persona(&args.graphql, args.home.as_deref(), record).await?;
    print_json(&json!({"status":"applied", "request_key":request_key, "behavior_id":behavior_id}))
}

pub(super) async fn behavior_clone(args: BehaviorCloneArgs) -> Result<()> {
    let agent_did = local_identity(args.home.as_deref())?.did().to_owned();
    require_source_behavior(&args.graphql, &agent_did, &args.source_behavior_id).await?;
    // Profile is required, no implicit fallback: the materializer validates the
    // published profile under the target agent's scope.
    let record = local_record(
        agent_did,
        "create",
        None,
        Some(args.source_behavior_id.clone()),
        Some(args.display_name.clone()),
        args.root.clone(),
        None,
        Some(args.profile_id.clone()),
    );
    let request_key = record.request_key.clone();
    let behavior_id = submit_local_persona(&args.graphql, args.home.as_deref(), record).await?;
    print_json(&json!({"status":"applied", "request_key":request_key, "behavior_id":behavior_id}))
}

pub(super) async fn behavior_disable(args: BehaviorDisableArgs) -> Result<()> {
    let agent_did = local_identity(args.home.as_deref())?.did().to_owned();
    require_source_behavior(&args.graphql, &agent_did, &args.behavior_id).await?;
    let record = local_record(
        agent_did,
        "disable",
        Some(args.behavior_id.clone()),
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
// (the linked AgentContext's prompt/skills, its referenced Tools groups with
// the preset classification and root, and the referenced InferenceProfile).
// Read-only, so this reads straight through `ConfigAccess`/`graphql_rows`
// rather than the persona-request channel above.

async fn load_document(
    access: &ConfigAccess,
    collection: Collection,
    fields: &str,
    owner: Option<&str>,
    id: &str,
) -> Result<Option<Value>> {
    let escaped_id = escape_graphql_string(id);
    let filter = match owner {
        Some(owner) => format!(
            r#"agent_did: {{ _eq: "{}" }}, "#,
            escape_graphql_string(owner)
        ),
        None => String::new(),
    };
    let query = format!(
        r#"{{
            {collection_type}(filter: {{ {owner_filter}{unique_field}: {{ _eq: "{escaped_id}" }} }}, limit: 2) {{
                {fields}
            }}
        }}"#,
        collection_type = collection.graphql_type(),
        owner_filter = filter,
        unique_field = collection.unique_field(),
    );
    let rows = graphql_rows(access, collection.graphql_type(), &query).await?;
    // Exact scoped lookup: a logical id resolving to more than one live row
    // (unscoped lookup or duplicate scoped key) is ambiguous, not selectable.
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous {} {id:?}: {} live rows resolve this logical id",
        collection.graphql_type(),
        rows.len()
    );
    Ok(rows.into_iter().next())
}

async fn classify_preset(access: &ConfigAccess, value: &Value) -> Result<Option<&'static str>> {
    use gents::document_config::{
        merge_datastore_tool_surfaces, DatastoreToolSurfaceDocument, Tools,
    };
    let tools: Tools = serde_json::from_value(value.clone())?;
    tools.validate()?;
    let mut surfaces = Vec::<DatastoreToolSurfaceDocument>::new();
    for id in tools
        .datastore
        .as_ref()
        .and_then(|d| d.datastore_tool_surface_ids.as_ref())
        .into_iter()
        .flatten()
    {
        let row = load_document(
            access,
            Collection::DatastoreToolSurface,
            "surface_id agent_did display_name enabled entries created_at tags",
            Some(&tools.agent_did),
            id,
        )
        .await?
        .with_context(|| format!("missing selected datastore surface {id:?}"))?;
        surfaces.push(serde_json::from_value(row)?);
    }
    let merged = merge_datastore_tool_surfaces(&tools, &surfaces)?;
    persona_presets::classify_tools(&tools, &merged)
}

const CANONICAL_BEHAVIOR_SHOW_FIELDS: &str = "behavior_id agent_did display_name description context_id inference_profile_id enabled tags created_at";

const CANONICAL_PROFILE_SHOW_FIELDS: &str = "profile_id display_name description backend_id model_name reasoning_effort context_window max_output_tokens sampling_id execution_id tags";

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
        CANONICAL_BEHAVIOR_SHOW_FIELDS,
        None,
        &id,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("not found: no AgentBehavior document with behavior_id {id}"))?;

    let agent_did = row
        .get("agent_did")
        .and_then(Value::as_str)
        .context("AgentBehavior is missing its owner DID")?
        .to_string();
    let context_id = row
        .get("context_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    let profile_id = row
        .get("inference_profile_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);

    let context = match context_id.as_deref() {
        Some(context_id) => {
            load_document(
                &access,
                Collection::AgentContext,
                "context_id agent_did display_name description system_prompt tools_id compaction_id skill_ids tags",
                Some(&agent_did),
                context_id,
            )
            .await?
        }
        None => None,
    };
    let tools = match context
        .as_ref()
        .and_then(|context| context.get("tools_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        Some(tools_id) => {
            load_document(
                &access,
                Collection::Tools,
                "tools_id agent_did display_name host remote subagents built_ins datastore integrations self_config tags",
                Some(&agent_did),
                tools_id,
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
                CANONICAL_PROFILE_SHOW_FIELDS,
                Some(&agent_did),
                profile_id,
            )
            .await?
        }
        None => None,
    };
    let preset_name = match tools.as_ref() {
        Some(tools) => Some(classify_preset(&access, tools).await?.unwrap_or("custom")),
        None => None,
    };
    let root = tools
        .as_ref()
        .and_then(|tools| tools.get("host"))
        .and_then(|host| host.get("root"))
        .cloned()
        .unwrap_or(Value::Null);

    let resolved = json!({
        "root": root,
        "preset_name": preset_name,
        "profile": profile,
        "context": context,
        "tools": tools,
    });
    if let Value::Object(ref mut map) = row {
        map.insert("resolved".to_string(), resolved);
    }
    print_json(&row)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use gents::agent::persona_ops::{
        apply_persona_request, PersonaCatalogView, PersonaOp, PersonaRequestDoc,
    };
    use gents::document_config::Tools;
    use gents::{ensure_runtime_schemas, upsert_agent_behavior};
    use serde_json::Value;

    use super::*;

    /// Read one canonical document by its scoped logical key through the raw
    /// GraphQL door. Tests only; production code reads through the shared
    /// scoped readers.
    async fn read_canonical(
        node: &EmbeddedNode,
        collection: &str,
        id_field: &str,
        owner: &str,
        id: &str,
        extra_fields: &str,
    ) -> Result<Option<Value>> {
        let query = format!(
            r#"{{ {collection}(filter: {{ agent_did: {{ _eq: "{}" }}, {id_field}: {{ _eq: "{}" }} }}, limit: 2) {{ {id_field} agent_did {extra_fields} }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(id),
        );
        let response = node.execute(&query).await;
        anyhow::ensure!(!response.has_errors(), "query {collection} failed");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        anyhow::ensure!(
            rows.len() <= 1,
            "ambiguous {collection} {id:?} under {owner:?}"
        );
        Ok(rows.into_iter().next())
    }

    /// Canonical persona materialization contract exercised through the public
    /// `apply_persona_request` entry point (the same one this crate's
    /// `behavior create`/`clone` commands drive): a profile-only create mints
    /// the canonical Behavior -> AgentContext -> Tools chain with no
    /// backend/model copy on the behavior, and a clone copies the source
    /// context/tools into private documents while switching the profile.
    #[tokio::test]
    async fn persona_create_and_clone_materialize_canonical_chain() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await
                .expect("embedded node boots"),
        );
        ensure_runtime_schemas(&node).await?;
        let owner = "did:key:persona-owner";
        gents::ensure_agent_principal(&node, owner).await?;
        use gents::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let documents = [
            (Collection::InferenceBackend,json!({"agent_did":owner,"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
            (Collection::InferenceProfile,json!({"agent_did":owner,"profile_id":"profile-1","backend_id":"backend","model_name":"test"})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument{collection,add:value.clone(),update:value}).collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        ConfigAccess::Local(node.clone())
            .transact("test.persona.profile", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await?;

        let doc = PersonaRequestDoc {
            request_key: "canonical-create-1".to_string(),
            agent_did: owner.to_string(),
            op_raw: "create".to_string(),
            op: Some(PersonaOp::Create { clone_from: None }),
            persona_name: Some("Canonical".to_string()),
            root: Some("".to_string()),
            preset: Some("write".to_string()),
            profile_id: Some("profile-1".to_string()),
            ..Default::default()
        };
        let outcome = apply_persona_request(&node, &doc, &PersonaCatalogView::default()).await?;
        assert!(!outcome.repaired);

        let behavior = gents::load_agent_behavior(&node, &outcome.behavior_id)
            .await?
            .expect("created behavior exists");
        assert_eq!(behavior.inference_profile_id, "profile-1");
        let context_id = behavior.context_id.clone().expect("create mints a context");
        let context_row = read_canonical(
            &node,
            "AgentContext",
            "context_id",
            owner,
            &context_id,
            "tools_id",
        )
        .await?
        .expect("created context exists");
        let tools_id = context_row
            .get("tools_id")
            .and_then(Value::as_str)
            .expect("write preset mints tools")
            .to_string();
        let tools_row = read_canonical(&node, "Tools", "tools_id", owner, &tools_id, "host")
            .await?
            .expect("created tools exist");
        let tools: Tools = serde_json::from_value(tools_row.clone())?;
        assert_eq!(
            tools
                .host
                .as_ref()
                .and_then(|host| host.bash.as_ref())
                .map(|bash| bash.mode),
            Some(gents::tool_surface::BashMode::Unrestricted),
        );

        // Clone-by-create: private context/tools copies, new profile selection.
        let clone_doc = PersonaRequestDoc {
            request_key: "canonical-clone-1".to_string(),
            agent_did: owner.to_string(),
            op_raw: "create".to_string(),
            op: Some(PersonaOp::Create {
                clone_from: Some(outcome.behavior_id.clone()),
            }),
            persona_name: Some("Cloned".to_string()),
            root: None,
            preset: None,
            profile_id: Some("profile-1".to_string()),
            ..Default::default()
        };
        let clone_outcome =
            apply_persona_request(&node, &clone_doc, &PersonaCatalogView::default()).await?;
        assert!(!clone_outcome.repaired);
        assert_ne!(clone_outcome.behavior_id, outcome.behavior_id);
        let cloned = gents::load_agent_behavior(&node, &clone_outcome.behavior_id)
            .await?
            .expect("cloned behavior exists");
        let cloned_context_id = cloned
            .context_id
            .clone()
            .expect("clone mints a private context");
        assert_ne!(cloned_context_id, context_id);
        let cloned_context_row = read_canonical(
            &node,
            "AgentContext",
            "context_id",
            owner,
            &cloned_context_id,
            "tools_id",
        )
        .await?
        .expect("cloned context exists");
        let cloned_tools_id = cloned_context_row
            .get("tools_id")
            .and_then(Value::as_str)
            .expect("cloned tools")
            .to_string();
        assert_ne!(
            cloned_tools_id, tools_id,
            "clone must not mutate the shared source tools"
        );
        let access = ConfigAccess::Local(node.clone());
        assert!(load_document(
            &access,
            Collection::AgentBehavior,
            "behavior_id agent_did",
            Some("did:key:foreign"),
            &outcome.behavior_id
        )
        .await?
        .is_none());
        assert!(load_document(
            &access,
            Collection::AgentBehavior,
            "behavior_id agent_did",
            Some(owner),
            &outcome.behavior_id
        )
        .await?
        .is_some());
        let preset = classify_preset(&access, &tools_row).await?;
        assert_eq!(preset, Some("write"));
        // The source chain is untouched by the clone.
        let source_tools = read_canonical(&node, "Tools", "tools_id", owner, &tools_id, "host")
            .await?
            .expect("source tools unchanged");
        assert_eq!(source_tools.get("tools_id"), Some(&json!(tools_id)));
        Ok(())
    }

    /// The raw `behavior set` door is a complete canonical replacement through
    /// the shared writer: omitted optionals clear and a dangling
    /// context/profile reference fails the write transaction.
    #[tokio::test]
    async fn raw_set_rejects_dangling_profile_reference() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await
                .expect("embedded node boots"),
        );
        ensure_runtime_schemas(&node).await?;
        gents::ensure_agent_principal(&node, "did:key:set-owner").await?;
        let behavior = AgentBehavior {
            behavior_id: "b1".to_string(),
            agent_did: "did:key:set-owner".to_string(),
            display_name: None,
            description: None,
            context_id: None,
            inference_profile_id: "missing-profile".to_string(),
            enabled: true,
            tags: Vec::new(),
            created_at: None,
        };
        // The shared writer behavior_set drives owns the reference validation;
        // a dangling profile rejects inside its transaction.
        assert!(format!(
            "{:#}",
            upsert_agent_behavior(&node, &behavior).await.unwrap_err()
        )
        .contains("missing-profile"));
        // Nothing was published: the canonical chain stays empty.
        let response = node
            .execute("{ AgentBehavior {behavior_id} AgentContext {context_id} Tools {tools_id} }")
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        for name in ["AgentBehavior", "AgentContext", "Tools"] {
            assert_eq!(
                response.data.as_ref().unwrap()[name],
                serde_json::json!([]),
                "{name} must stay empty after a rejected write"
            );
        }
        Ok(())
    }
}
