use super::*;

pub(super) fn resource_target(resource: &str) -> Option<SelfConfigTarget> {
    cleanup_target(resource).ok()
}

impl ConfigCommandTool {
    /// Familiar resource verbs adapt to the existing patch, agent and cleanup
    /// owners. They neither widen category grants nor bypass per-resource guards.
    pub(super) async fn crud(&self, argv: &[String]) -> Result<Option<String>> {
        let Some(resource) = argv.first().map(String::as_str) else {
            return Ok(None);
        };
        let Some(target) = resource_target(resource) else {
            return Ok(None);
        };
        let preview = argv.get(1).is_some_and(|word| word == "preview");
        let offset = if preview { 2 } else { 1 };
        let Some(verb) = argv.get(offset).map(String::as_str) else {
            return Ok(None);
        };
        let args = &argv[offset + 1..];
        let exact = args.first().is_some_and(|word| !word.starts_with('-'));
        if verb == "delete" {
            let id = required_resource_id(args.first(), "document ID")?;
            let parsed = ParsedArgs::parse(&args[1..])?;
            if !preview && parsed.one("digest")?.is_none() {
                return Err(CommandGuidance {
                    message: format!("{resource} delete requires options.digest from preview delete; review that result before applying its returned delete call"),
                    next_call: json!({"argv":[resource,"preview","delete"],"target_id":id}),
                }.into());
            }
            let mut cleanup = vec![
                if preview { "preview" } else { "remove" }.into(),
                "--target".into(),
                format!("{resource}={id}"),
            ];
            cleanup.extend_from_slice(&args[1..]);
            let mut receipt: Value = serde_json::from_str(&self.cleanup(&cleanup).await?)?;
            receipt["operation"] = json!(if preview { "preview delete" } else { "delete" });
            if preview {
                receipt["effect"] = json!("No documents changed. Use the returned delete call; it rechecks content and references before committing.");
                receipt["apply_with"] = json!({"argv":[resource,"delete"],"target_id":id,"options":{"digest":receipt["plan_digest"]}});
            }
            return Ordered::reading_order(
                receipt,
                &[
                    "committed",
                    "operation",
                    "targets",
                    "effect",
                    "apply_with",
                    "plan_digest",
                    "owner",
                ],
            )
            .pretty()
            .map(Some);
        }
        if target == SelfConfigTarget::Agent {
            if verb == "update" {
                let mut old = if preview {
                    vec!["preview".into(), "edit".into()]
                } else {
                    vec!["edit".into()]
                };
                old.extend_from_slice(args);
                return self.agent(&old).await.map(Some);
            }
            return Ok(None);
        }
        let standalone = matches!(
            resource,
            "context"
                | "sampling"
                | "retry-policy"
                | "compaction"
                | "task"
                | "trigger"
                | "schedule"
                | "event-source"
                | "execution"
                | "agent-target"
        );
        let handled = match verb {
            "list" | "update" => true,
            "get" | "edit" => standalone,
            "create" => {
                standalone
                    || matches!(resource, "tools" | "skill" | "mcp-service")
                    || resource == "backend" && exact && args.iter().any(|arg| arg == "--set")
            }
            _ => false,
        };
        if !handled {
            return Ok(None);
        }
        if verb == "create" && !exact {
            bail!("{resource} create requires a new document ID in target_id or after create in argv; fields go in set");
        }
        if !exact
            && (verb == "update" && matches!(resource, "context" | "tools")
                || verb == "get" && resource == "context")
        {
            if resource != "context" {
                self.ensure_resource(target.category())?;
            }
        } else {
            self.ensure_crud_resource(target)?;
        }
        if verb == "list" {
            anyhow::ensure!(!preview, "list is read-only; omit preview");
            let parsed = ParsedArgs::parse(args)?;
            anyhow::ensure!(
                parsed.positionals.is_empty()
                    && parsed.switches.is_empty()
                    && parsed
                        .options
                        .keys()
                        .all(|key| matches!(key.as_str(), "limit" | "cursor")),
                "{resource} list accepts only options.limit and options.cursor"
            );
            return self
                .inference_inventory(target, parse_limit(&parsed)?, parsed.one("cursor")?)
                .await
                .map(Some);
        }
        if verb == "get" && !exact && resource == "context" {
            anyhow::ensure!(!preview, "get is read-only; omit preview");
            let mut bound = vec!["get".into()];
            bound.extend_from_slice(args);
            return self.agent_context(&bound).await.map(Some);
        }
        if verb == "get" {
            anyhow::ensure!(
                !preview && args.len() == 1,
                "{resource} get requires target_id or one document ID after get in argv; use {resource} list to discover IDs"
            );
            return self.exact_read(target, &args[0]).await.map(Some);
        }
        if !exact
            && matches!(resource, "tools" | "context" | "profile" | "backend")
            && verb == "update"
        {
            let mut old = vec![if preview { "preview" } else { "edit" }.into()];
            old.extend_from_slice(args);
            return match resource {
                "context" => self.agent_context(&old).await,
                "tools" => self.bound_document(resource, &old).await,
                "profile" => self.profile(&old).await,
                "backend" => self.backend(&old).await,
                _ => unreachable!(),
            }
            .map(Some);
        }
        let id = required_resource_id(args.first(), "document ID")?.clone();
        let create = verb == "create";
        let (agent, rest) = extract_agent_target(&args[1..])?;
        let (allow_drop, rest) = extract_allow_drop(&rest)?;
        anyhow::ensure!(
            allow_drop.is_empty() || target == SelfConfigTarget::Tools,
            "allow-drop applies only to tools"
        );
        let mut patch = if create && rest.is_empty() {
            Vec::new()
        } else if target == SelfConfigTarget::DatastoreToolSurface {
            datastore::surface_patch(&rest)?
        } else {
            parse_patch(&rest, target)?
        };
        if target == SelfConfigTarget::AgentTarget {
            if create && !patch.iter().any(|(field, _)| field == "target_node_did") {
                patch.push(("target_node_did".into(), Some(json!(self.node_did))));
            }
            self.resolve_target_agent(&id, &mut patch).await?;
        }
        let mut core = if matches!(
            target,
            SelfConfigTarget::Task
                | SelfConfigTarget::Trigger
                | SelfConfigTarget::Schedule
                | SelfConfigTarget::EventSource
        ) {
            self.automation_core(target, &id, create, agent.as_deref(), &patch)
                .await?
        } else {
            anyhow::ensure!(agent.is_none(), "{resource} with an exact ID does not accept options.agent; the ID identifies the document");
            self.core.clone()
        };
        let contextual = matches!(
            target,
            SelfConfigTarget::AgentContext | SelfConfigTarget::Tools
        );
        let mut bound = false;
        if contextual && !create {
            if let Some(agent) = self.document_agent(target, &id).await? {
                core = self.target_core(Some(&agent), resource).await?;
                bound = true;
            }
        }
        let mut request = match target {
            SelfConfigTarget::Tools => {
                refuse_silent_tools_drops(tools_request(&core, patch), allow_drop)
            }
            SelfConfigTarget::InferenceBackend if create => {
                let mut request = local_backend_create_request(
                    self.node_did.clone(),
                    id.clone(),
                    String::new(),
                    None,
                    None,
                );
                request.patch.extend(patch);
                request
            }
            SelfConfigTarget::InferenceBackend => backend_request(patch),
            SelfConfigTarget::Task
            | SelfConfigTarget::Trigger
            | SelfConfigTarget::Schedule
            | SelfConfigTarget::EventSource => automation_request(&core, target, id.clone(), patch),
            _ => ApplyRequest::new(target, patch),
        };
        if contextual {
            if bound {
                request = protect_working_agent(request);
            }
            let validate = request.validate;
            let expected_id = id.clone();
            request.validate = Box::new(move |txn, anchor, stored, merged| {
                let validation = validate(txn, anchor, stored, merged);
                let id = expected_id.clone();
                Box::pin(async move {
                    if !create {
                        if bound {
                            anyhow::ensure!(anchor.ref_id(target.unique_field()).as_deref() == Some(id.as_str()), "document binding changed; get the agent again before updating {id:?}");
                        } else {
                            let owner = anchor
                                .doc
                                .get("node_did")
                                .and_then(Value::as_str)
                                .context("missing owner")?;
                            anyhow::ensure!(document_agent_in_txn(txn, target, owner, &id).await?.is_none(), "document {id:?} was selected during this call; retry the update against its current agent");
                        }
                    }
                    validation.await
                })
            });
            // An unselected or newly created document cannot affect the
            // invoking chain. Bound updates keep its existing guard slots.
            request.guard_selected_chain = bound;
        }
        if target == SelfConfigTarget::InferenceBackend {
            let guard = request.guard;
            let guarded_id = id.clone();
            request.guard = Box::new(move |anchor, stored, merged| {
                if anchor.ref_id("backend_id").as_deref() == Some(guarded_id.as_str()) {
                    guard(anchor, stored, merged)
                } else {
                    Ok(())
                }
            });
        }
        request.allow_create = create;
        request.require_create = create;
        let unique = id.clone();
        request.resolve_unique = Box::new(move |_| Ok(unique.clone()));
        if !matches!(
            target,
            SelfConfigTarget::Task
                | SelfConfigTarget::Trigger
                | SelfConfigTarget::Schedule
                | SelfConfigTarget::EventSource
        ) {
            let owner = self.node_did.clone();
            request.on_create = Box::new(move |id, doc| {
                doc.insert(target.unique_field().into(), json!(id));
                doc.insert("node_did".into(), json!(owner));
                Ok(())
            });
        }
        self.patch(&core, if preview { "preview" } else { "edit" }, request)
            .await
            .map(Some)
    }

    /// Exact-ID routing chooses the existing owner; automation_request rechecks
    /// that ownership in the write transaction, including both trigger targets.
    async fn automation_core(
        &self,
        target: SelfConfigTarget,
        id: &str,
        create: bool,
        agent: Option<&str>,
        patch: &SelfConfigPatch,
    ) -> Result<SelfConfigCore> {
        if agent.is_some()
            || !matches!(target, SelfConfigTarget::Task | SelfConfigTarget::Trigger)
            || target == SelfConfigTarget::Task && create
        {
            return self.target_core(agent, "automation").await;
        }
        let owner = &self.node_did;
        let task = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.automation_owner",
            |txn| {
                Box::pin(async move {
                    if target == SelfConfigTarget::Task {
                        return ops::read_owned_doc(txn, target, owner, id).await;
                    }
                    let stored = if create {
                        None
                    } else {
                        ops::read_owned_doc(txn, target, owner, id).await?
                    };
                    let task_id = if create {
                        patch
                            .iter()
                            .rev()
                            .find(|(field, _)| field == "task_id")
                            .and_then(|(_, value)| value.as_ref())
                            .and_then(Value::as_str)
                    } else {
                        stored
                            .as_ref()
                            .and_then(|(_, doc)| doc.get("task_id"))
                            .and_then(Value::as_str)
                    };
                    match task_id {
                        Some(task_id) => {
                            ops::read_owned_doc(txn, SelfConfigTarget::Task, owner, task_id).await
                        }
                        None => Ok(None),
                    }
                })
            },
        )
        .await?;
        let agent = task
            .as_ref()
            .and_then(|(_, doc)| doc.get("agent_id"))
            .and_then(Value::as_str);
        self.target_core(agent, "automation").await
    }

    fn ensure_crud_resource(&self, target: SelfConfigTarget) -> Result<()> {
        if matches!(
            target,
            SelfConfigTarget::AgentContext | SelfConfigTarget::Tools
        ) {
            anyhow::ensure!(self.categories.contains("node"), "exact-ID context/tools operations require the agent catalog grant; use options.agent to edit your selected configuration");
        }
        if target == SelfConfigTarget::AgentContext {
            return Ok(());
        }
        self.ensure_resource(match target.category() {
            "mcp_service" => "mcp-service",
            other => other,
        })
    }

    async fn document_agent(&self, target: SelfConfigTarget, id: &str) -> Result<Option<String>> {
        let owner = &self.node_did;
        crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.document_agent",
            |txn| Box::pin(async move { document_agent_in_txn(txn, target, owner, id).await }),
        )
        .await
    }

    /// A batch is an ordered composition of existing transactions, not a new
    /// write owner. Failed item i leaves items before i committed and later
    /// items unattempted; callers receive every attempted item's receipt.
    pub(super) async fn batch(&self, argv: &[String]) -> Result<String> {
        let parsed = ParsedArgs::parse(&argv[1..])?;
        anyhow::ensure!(
            parsed.positionals.is_empty()
                && parsed.switches.is_empty()
                && parsed.options.keys().all(|key| key == "operations"),
            "batch accepts only options.operations: an array of config calls"
        );
        let source = parsed
            .one("operations")?
            .context("batch requires options.operations: an array of config calls")?;
        anyhow::ensure!(source.len() <= 128 * 1024, "batch is limited to 128 KiB");
        let operations: Vec<ConfigCommandParams> = serde_json::from_str(source).context("options.operations must be an array of config calls with argv, target_id, set, clear and options")?;
        anyhow::ensure!(
            !operations.is_empty() && operations.len() <= 64,
            "batch requires 1..64 operations"
        );
        let commands = operations
            .into_iter()
            .enumerate()
            .map(|(index, call)| {
                let argv = call
                    .into_argv()
                    .with_context(|| format!("batch item {index}; no operations ran"))?;
                anyhow::ensure!(
                    argv.first().is_none_or(|word| word != "batch"),
                    "batch item {index}: nested batches are not supported; no operations ran"
                );
                Ok(argv)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut results = Vec::new();
        for (index, argv) in commands.iter().enumerate() {
            let child = Self {
                execution: Arc::new(super::super::execution::ExecutionObservation::default()),
                ..self.clone()
            };
            let result = Box::pin(child.dispatch(argv)).await;
            log_config_call(
                argv,
                &json!({"argv":argv}).to_string(),
                config_help_resource(argv).is_some(),
                argv.iter().any(|word| word == "preview"),
                &result,
            );
            let receipt = child.execution.receipt();
            if receipt.mutation_entered {
                self.execution.enter_mutation();
            }
            match result {
                Ok(text) => results.push(json!({"index":index,"ok":true,"result":serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)),"config_execution":receipt})),
                Err(error) => {
                    let deleting = argv.first().is_some_and(|word| word == "cleanup") || argv.iter().take(3).any(|word| word == "delete");
                    let (message, recovery) = child.failure_guidance(&error, argv, deleting);
                    results.push(json!({"index":index,"ok":false,"error":message,"recovery":recovery,"config_execution":receipt}));
                    return Err(BatchFailure { results, failed_index:index, unattempted:commands.len()-index-1 }.into());
                }
            }
        }
        ordered! {"completed":true,"atomic":false,"results":results}.pretty()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("batch stopped at item {failed_index}; earlier items keep their results; {unattempted} later items were not attempted")]
pub(super) struct BatchFailure {
    pub results: Vec<Value>,
    pub failed_index: usize,
    pub unattempted: usize,
}

async fn document_referrers(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
) -> Result<Vec<Value>> {
    let (collection, field, key) = if target == SelfConfigTarget::AgentContext {
        ("Agent", "context_id", "agent_id")
    } else {
        ("AgentContext", "tools_id", "context_id")
    };
    let query = format!("{{ {collection}(filter: {{node_did: {{_eq: \"{}\"}}, {field}: {{_eq: \"{}\"}}}}) {{{key}}} }}", escape_graphql_string(owner), escape_graphql_string(id));
    let response = txn.execute(&query).await?;
    response["data"][collection]
        .as_array()
        .cloned()
        .context("reference query missing rows")
}

async fn document_agent_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
) -> Result<Option<String>> {
    let rows = document_referrers(txn, target, owner, id).await?;
    anyhow::ensure!(rows.len() <= 1, "targeted configuration requires an unshared Context and Tools; clone the working agent before editing shared configuration");
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    if target == SelfConfigTarget::AgentContext {
        return Ok(Some(
            row["agent_id"]
                .as_str()
                .context("referring agent ID missing")?
                .to_owned(),
        ));
    }
    let context = row["context_id"]
        .as_str()
        .context("referring context ID missing")?;
    let agents = document_referrers(txn, SelfConfigTarget::AgentContext, owner, context).await?;
    anyhow::ensure!(agents.len() <= 1, "targeted configuration requires an unshared Context and Tools; clone the working agent before editing shared configuration");
    agents
        .first()
        .map(|row| {
            row["agent_id"]
                .as_str()
                .map(str::to_owned)
                .context("referring agent ID missing")
        })
        .transpose()
}
