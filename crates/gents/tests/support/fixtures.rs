use std::sync::Arc;
use std::time::Duration;

use gents::{
    AgentToolSurfaceConfig, BackendProviderKind, KeyIdentity, NodeIdentity, ResolvedAgent,
    RuntimeNode,
};

pub fn fixture_pack_names() -> Vec<String> {
    const KNOWN: [&str; 8] = [
        "assets_fixture",
        "documents_fixture",
        "slot_fixture",
        "dependent_fixture",
        "dependee_fixture",
        "bind_plugin_fixture",
        "job_plugin_fixture",
        "plugin_documents_fixture",
    ];
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("reading {}: {error}", root.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("reading an entry of {}: {error}", root.display()))
            .path();
        if !path.is_dir() {
            continue;
        }
        let manifest = path.join("manifest.json");
        let bytes = std::fs::read(&manifest)
            .unwrap_or_else(|error| panic!("reading {}: {error}", manifest.display()));
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", manifest.display()));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} is not a UTF-8 name", path.display()));
        names.push(name.to_owned());
    }
    names.sort();
    for known in KNOWN {
        assert!(
            names.iter().any(|name| name == known),
            "fixture {known} is missing from {}",
            root.display()
        );
    }
    names
}

pub fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

pub fn test_node_for(
    identity: Arc<dyn gents::NodeIdentity>,
    default_agent_did: impl Into<String>,
) -> Arc<RuntimeNode> {
    Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: default_agent_did.into(),
        display_name: None,
        enabled: true,
    })
}

pub fn test_agent(
    name: &str,
    backend_id: &str,
    backend_api_key_env_var: Option<&str>,
) -> ResolvedAgent {
    let identity: Arc<dyn gents::NodeIdentity> = Arc::new(test_identity(name));
    let principal = test_node_for(identity, name);
    let mut behavior = test_agent_for_node(name, principal);
    behavior.backend_id = Some(backend_id.to_owned());
    behavior.backend_auth = match backend_api_key_env_var {
        Some(variable) => gents::documents::BackendAuth::Environment {
            variable: variable.into(),
        },
        None => gents::documents::BackendAuth::Unauthenticated,
    };
    behavior
}

pub fn test_agent_for_node(
    agent_did: impl Into<String>,
    principal: Arc<RuntimeNode>,
) -> ResolvedAgent {
    let _display_name = agent_did.into();
    let binding =
        gents::store::identity::provision_agent_identity(principal.identity.as_ref()).unwrap();
    let agent_identity =
        gents::store::identity::resolve_agent_identity(principal.identity.as_ref(), &binding)
            .unwrap();
    test_agent_with_identity(principal, agent_identity)
}

pub fn test_agent_with_identity(
    principal: Arc<RuntimeNode>,
    agent_identity: Arc<dyn gents::NodeIdentity>,
) -> ResolvedAgent {
    let agent_did = agent_identity.did().to_owned();
    ResolvedAgent {
        configuration: gents::inference::resolved::ResolvedConfiguration::Static,
        profile_id: None,
        skills: Vec::new(),
        oauth_pool: Vec::new(),
        agent_identity,
        agent_did,
        node: principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        claude_platform: None,
        backend_auth: gents::documents::BackendAuth::Unauthenticated,
        model_name: gents::inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: gents::inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents::inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents::inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents::inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        tools: AgentToolSurfaceConfig::default(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: gents::inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents::inference::resolved::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            gents::inference::resolved::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        provider_idle_timeout: Duration::from_secs(
            gents::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry:
            gents::runtime::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: gents::inference::resolved::SamplingConfig::default(),
    }
}

pub async fn session_command_agent_snapshot(
    db: &super::TestDb,
) -> tokio::sync::watch::Receiver<
    Arc<gents::runtime::agent::p2p_reconcile::session_commands::SessionCommandAgentSnapshot>,
> {
    let access =
        gents::ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
    let response = access
        .execute("{ Agent { agent_did creation_nonce node_did } }")
        .await
        .unwrap();
    let mut agents = rapidhash::RapidHashMap::default();
    for row in response["data"]["Agent"].as_array().expect("Agent rows") {
        if row["node_did"].as_str() != Some(db.node_identity.did()) {
            continue;
        }
        let binding = gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: row["agent_did"].as_str().expect("Agent DID").into(),
            creation_nonce: row["creation_nonce"]
                .as_str()
                .expect("Agent creation nonce")
                .into(),
        };
        let signer =
            gents::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .expect("held Agent identity");
        let node = test_node_for(db.node_identity.clone(), &binding.agent_did);
        let agent = Arc::new(test_agent_with_identity(node, signer));
        agents.insert(binding.agent_did, agent);
    }
    assert!(
        !agents.is_empty(),
        "session command tests require a held Agent"
    );
    let default_agent_did = agents.keys().next().unwrap().clone();
    let runtime_node = test_node_for(db.node_identity.clone(), &default_agent_did);
    let snapshot =
        gents::runtime::agent::p2p_reconcile::session_commands::SessionCommandAgentSnapshot {
            generation: 1,
            node: Some(runtime_node),
            local_did: db.node_identity.did().into(),
            default_agent_did,
            agents,
            tool_surfaces: Default::default(),
            backend_admission_configs: Default::default(),
            unavailable_agents: Default::default(),
            active_schedules: Default::default(),
            unavailable_schedules: Default::default(),
            active_event_triggers: Default::default(),
            unavailable_event_triggers: Default::default(),
            active_tasks: Default::default(),
            dispatchers: Default::default(),
            agent_executor_capacities: Default::default(),
            agent_executor_queue_capacities: Default::default(),
        };
    let (_sender, receiver) = tokio::sync::watch::channel(Arc::new(snapshot));
    receiver
}

/// Configures an Agent's tools as the Node's retained configuring User.
pub async fn configure_agent_tools(
    db: &super::TestDb,
    node_did: &str,
    agent_did: &str,
    system_prompt: Option<String>,
    tools: gents::documents::Tools,
    referenced_documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    let user = super::load_managed_fixture_user(db).await;
    configure_agent_tools_as(
        &db.client_access(&user),
        node_did,
        agent_did,
        system_prompt,
        tools,
        referenced_documents,
    )
    .await;
}

pub async fn configure_agent_tools_as(
    access: &gents::ConfigAccess,
    node_did: &str,
    agent_did: &str,
    system_prompt: Option<String>,
    tools: gents::documents::Tools,
    referenced_documents: Vec<(gents::Collection, serde_json::Value)>,
) {
    use gents::documents::AgentContext;
    use gents::store::config_client::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents::Collection;

    assert_eq!(tools.node_did, node_did);
    let mut tools = tools;
    if let Some(host) = tools.host.as_mut() {
        // Under document ACP the WorkspaceRoot policy is always explicit: host
        // tools run only under a User-published root their Agent may read. An
        // unset root keeps its former meaning, the runtime cwd, made explicit.
        let root = match host.root.as_deref() {
            Some(root) => std::path::PathBuf::from(root),
            None => std::env::current_dir().expect("runtime cwd"),
        };
        let root = std::fs::canonicalize(&root)
            .expect("canonical tool root")
            .to_string_lossy()
            .into_owned();
        gents::tools::surface::set_workspace_root_with_readers(
            access,
            node_did,
            &root,
            None,
            true,
            &[agent_did.to_owned()],
        )
        .await
        .expect("publish the Agent's tool root");
        host.root = Some(root);
    }
    access
        .transact_write("test.configure_agent_tools", |txn| {
            let tools = tools.clone();
            let referenced_documents = referenced_documents.clone();
            let system_prompt = system_prompt.clone();
            Box::pin(async move {
                let (_, behavior) = read(txn, Collection::Agent, node_did, agent_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("behavior {agent_did} is missing"))?;
                let mut behavior: gents::AgentDocument = serde_json::from_value(behavior)?;
                let context_id = behavior
                    .context_id
                    .clone()
                    .unwrap_or_else(|| format!("{agent_did}:context"));
                let mut context = read(txn, Collection::AgentContext, node_did, &context_id)
                    .await?
                    .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                    .transpose()?
                    .unwrap_or_else(|| AgentContext {
                        context_id: context_id.clone(),
                        node_did: node_did.to_string(),
                        display_name: None,
                        description: None,
                        system_prompt: None,
                        tools_id: None,
                        compaction_id: None,
                        skill_ids: Vec::new(),
                        tags: Vec::new(),
                    });
                context.tools_id = Some(tools.tools_id.clone());
                if let Some(system_prompt) = system_prompt {
                    context.system_prompt = Some(system_prompt);
                }
                behavior.context_id = Some(context_id);

                let mut documents = referenced_documents;
                documents.extend([
                    (Collection::Tools, serde_json::to_value(tools)?),
                    (Collection::AgentContext, serde_json::to_value(context)?),
                    (Collection::Agent, serde_json::to_value(behavior)?),
                ]);
                let plan = DesiredStateApplyPlan::new(
                    documents
                        .into_iter()
                        .map(|(collection, value)| DesiredStateApplyDocument {
                            collection,
                            add: value.clone(),
                            update: value,
                        })
                        .collect(),
                )?;
                gents::store::config_client::apply_desired_state_plan(txn, &plan)
                    .await
                    .map(|_| ())
            })
        })
        .await
        .unwrap();
}

pub fn agent_target(
    owner_did: &str,
    name: impl Into<String>,
    target_node_did: impl Into<String>,
    agent_did: impl Into<String>,
) -> gents::AgentTargetDocument {
    let name = name.into();
    gents::AgentTargetDocument {
        target_id: name.clone(),
        node_did: owner_did.to_string(),
        target_node_did: target_node_did.into(),
        agent_did: agent_did.into(),
        name,
        description: None,
        tags: Vec::new(),
    }
}

/// Binds the Node's default Agent to a backend as the Node's configuring User,
/// enrolling that User, its Node and a default Agent when the store is fresh.
pub async fn bind_default_agent_backend(db: &super::TestDb, backend_id: &str, endpoint: &str) {
    bind_agent_backend_chain(db, None, backend_id, endpoint, "default").await;
}

pub async fn held_fixture_history(
    db: &super::TestDb,
    fixture: &super::ManagedAgentFixture,
    session_id: &str,
) -> Vec<gents::inference::llm::message::Message> {
    let session = gents::store::graphql::escape_graphql_string(session_id);
    let response = fixture.access.execute(&format!(
        r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}, order: {{ sequence: ASC }}) {{ _docID }} }}"#,
    )).await.expect("read fixture message headers");
    let mut messages = Vec::new();
    for header in response["data"]["AgentMessage"]
        .as_array()
        .expect("message headers")
    {
        let (_, message) = gents::session::load_canonical_message(
            &fixture.access,
            header["_docID"].as_str().expect("message document ID"),
            db.node_identity.did(),
            Some(fixture.user.did()),
        )
        .await
        .expect("read fixture canonical message");
        messages.push(message);
    }
    messages
}

pub async fn bind_managed_agent_backend(
    db: &super::TestDb,
    fixture: &super::ManagedAgentFixture,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    bind_selected_managed_agent_backend(
        db,
        fixture,
        &fixture.binding.agent_did,
        true,
        backend_id,
        endpoint,
        model_name,
    )
    .await;
}

pub async fn bind_selected_managed_agent_backend(
    db: &super::TestDb,
    fixture: &super::ManagedAgentFixture,
    agent_did: &str,
    make_default: bool,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    use gents::Collection;

    let owner = db.node_identity.did();
    fixture
        .access
        .transact_write("test.bind_managed_agent_backend", |txn| {
            Box::pin(async move {
                let (_, mut node) =
                    read_desired_state_record_in_txn(txn, Collection::Node, owner, owner)
                        .await?
                        .expect("held user must own the test Node");
                let (_, agent) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, owner, agent_did)
                        .await?
                        .expect("managed test Agent must exist");
                let profile_id = agent["inference_profile_id"]
                    .as_str()
                    .expect("managed Agent inference profile");
                if make_default {
                    node["default_agent_did"] = agent_did.into();
                }
                let profile = serde_json::json!({
                    "node_did": owner,
                    "profile_id": profile_id,
                    "backend_id": backend_id,
                    "model_name": model_name,
                });
                let backend = serde_json::json!({
                    "node_did": owner,
                    "backend_id": backend_id,
                    "name": backend_id,
                    "provider_kind": "OpenAiCompatible",
                    "openai_wire_api": "chat_completions",
                    "auth": {"kind": "unauthenticated"},
                    "endpoint": endpoint,
                    "max_concurrent": 1,
                    "max_queue_depth": 100,
                    "enabled": true,
                });
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::Node, node),
                        (Collection::InferenceBackend, backend),
                        (Collection::InferenceProfile, profile),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
        .expect("bind managed Agent backend as held user");
    gents::inference::backends::registry::set_backend_probe_status_as(
        &fixture.access,
        owner,
        backend_id,
        "healthy",
    )
    .await
    .expect("mark test backend healthy");
}

/// Binds an installed Agent to a backend as the Node's configuring User and
/// makes it the Node default.
pub async fn bind_agent_backend(
    db: &super::TestDb,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    bind_agent_backend_chain(db, Some(agent_did), backend_id, endpoint, model_name).await;
}

async fn bind_agent_backend_chain(
    db: &super::TestDb,
    agent_did: Option<&str>,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
) {
    let owner = db.node_identity.did();
    let user = super::managed_fixture_user(db).await;
    let access = db.client_access(&user);
    let read = |collection: gents::Collection, id: String| {
        let access = access.clone();
        async move {
            access
                .transact_readonly("test.bind_backend.read", |txn| {
                    let id = &id;
                    Box::pin(async move {
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn, collection, owner, id,
                        )
                        .await
                        .map(|record| record.map(|(_, value)| value))
                    })
                })
                .await
                .expect("read fixture configuration as its User")
        }
    };
    if read(gents::Collection::Node, owner.to_owned())
        .await
        .is_none()
    {
        let admin = gents::ConfigAccess::local_as(
            db.node.clone(),
            identity::Did::new(owner).expect("valid runtime DID"),
        );
        gents::store::identity::GrantSet::OperatorUser
            .grant(
                &admin,
                &identity::Did::new(user.did()).expect("valid user DID"),
            )
            .await
            .expect("grant fixture User operator access");
        let node = serde_json::json!({"node_did": owner});
        let plan = gents::store::config_client::DesiredStateApplyPlan::new(vec![
            gents::store::config_client::DesiredStateApplyDocument {
                collection: gents::Collection::Node,
                add: node.clone(),
                update: node,
            },
        ])
        .expect("render fixture Node");
        access
            .transact_write("test.bind_backend.node", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents::store::config_client::apply_desired_state_plan(txn, plan).await
                })
            })
            .await
            .expect("install fixture Node as its User");
    }
    let agent_did = match agent_did {
        Some(agent_did) => agent_did.to_owned(),
        None => {
            let node = read(gents::Collection::Node, owner.to_owned())
                .await
                .expect("fixture Node");
            match node["default_agent_did"]
                .as_str()
                .filter(|did| !did.is_empty())
            {
                Some(agent_did) => agent_did.to_owned(),
                None => {
                    super::install_managed_agent(db, &access, &user, "Default fixture agent")
                        .await
                        .1
                        .agent_did
                }
            }
        }
    };
    let fixture = super::ManagedAgentFixture {
        agent: gents::store::identity::resolve_agent_identity(
            db.node_identity.as_ref(),
            &gents_protocol::agent_identity::AgentIdentityBinding {
                agent_did: agent_did.clone(),
                creation_nonce: read(gents::Collection::Agent, agent_did.clone())
                    .await
                    .expect("fixture Agent is installed by its User")["creation_nonce"]
                    .as_str()
                    .expect("fixture Agent creation nonce")
                    .to_owned(),
            },
        )
        .expect("derive fixture Agent"),
        binding: gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: agent_did.clone(),
            creation_nonce: String::new(),
        },
        user,
        access,
    };
    bind_selected_managed_agent_backend(
        db, &fixture, &agent_did, true, backend_id, endpoint, model_name,
    )
    .await;
}

/// The session's working Agent as its managed signer, derived by the runtime
/// that holds the Node key, for owners that act only as that Agent.
pub async fn session_agent_identity(
    node: &std::sync::Arc<gents::defra_node::Node>,
    runtime: &dyn gents::NodeIdentity,
    node_did: &str,
    session_id: &str,
) -> std::sync::Arc<dyn gents::NodeIdentity> {
    let access = gents::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did()).expect("runtime DID"),
    );
    let response = access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ agent_did }} }}"#,
            gents::store::graphql::escape_graphql_string(node_did),
            gents::store::graphql::escape_graphql_string(session_id),
        ))
        .await
        .expect("read session Agent");
    let agent_did = response["data"]["AgentSession"][0]["agent_did"]
        .as_str()
        .expect("session has a working Agent")
        .to_owned();
    let agent = gents::load_agent(node, &agent_did)
        .await
        .expect("load session Agent")
        .expect("session Agent exists");
    gents::store::identity::resolve_agent_identity(
        runtime,
        &gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: agent.agent_did,
            creation_nonce: agent.creation_nonce,
        },
    )
    .expect("derive session Agent signer")
}

/// Rebinds an InferenceBackend's `auth` as its configuring User through the
/// client-signed desired-state owner; client endpoints refuse raw mutations.
pub async fn set_backend_auth_as(
    access: &gents::ConfigAccess,
    owner: &str,
    backend_id: &str,
    auth: serde_json::Value,
) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    access
        .transact_write("test.set_backend_auth", |txn| {
            let auth = auth.clone();
            Box::pin(async move {
                let (_, mut backend) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::InferenceBackend,
                    owner,
                    backend_id,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("backend {backend_id} is missing"))?;
                backend["auth"] = auth;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("rebind backend auth as its User");
}
