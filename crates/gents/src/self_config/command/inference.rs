use super::*;

impl ConfigCommandTool {
    pub(super) async fn bound_document(&self, resource: &str, argv: &[String]) -> Result<String> {
        self.ensure_resource(resource)?;
        let verb = argv.first().map(String::as_str).with_context(|| {
            format!("{resource} command is required; see [\"help\",\"{resource}\"]")
        })?;
        let target = match resource {
            "tools" => SelfConfigTarget::Tools,
            "backend" => SelfConfigTarget::InferenceBackend,
            _ => unreachable!("bound resource"),
        };
        match verb {
            "list" if resource == "backend" => {
                let parsed = ParsedArgs::parse(&argv[1..])?;
                parsed.reject_mutation_flags()?;
                self.inference_inventory(target, parse_limit(&parsed)?, parsed.one("cursor")?)
                    .await
            }
            "list" => bail!(
                "{resource} has no list: each agent has one; read it with {{\"argv\":[\"{resource}\",\"get\"],\"options\":{{\"agent\":\"AGENT_ID\"}}}} and find agents with [\"agent\",\"list\"]"
            ),
            "get" => {
                let (agent_id, rest) = extract_agent_target(&argv[1..])?;
                anyhow::ensure!(
                    rest.len() <= 1,
                    "{resource} get accepts at most one ID and --agent AGENT_ID"
                );
                let core = self.target_core(agent_id.as_deref(), resource).await?;
                match rest.first() {
                    Some(id) => self.exact_read(target, id).await,
                    None => self.bound_read(&core, target).await,
                }
            }
            "create" | "preview"
                if target == SelfConfigTarget::Tools
                    && (verb == "create" || argv.get(1).is_some_and(|word| word == "create")) =>
            {
                bail!(
                "tools are created with their agent (agent create); change an agent's tools with {{\"argv\":[\"tools\",\"edit\"],\"options\":{{\"agent\":\"AGENT_ID\"}},\"set\":{{\"GROUP\":VALUE}}}}"
                )
            }
            "preview" | "edit" => {
                let (agent_id, rest) = extract_agent_target(&argv[1..])?;
                let core = self.target_core(agent_id.as_deref(), resource).await?;
                let (allow_drop, rest) = extract_allow_drop(&rest)?;
                anyhow::ensure!(
                    allow_drop.is_empty() || target == SelfConfigTarget::Tools,
                    "allow-drop applies only to tools; see [\"help\",\"tools\"]"
                );
                let patch = parse_patch(&rest, target)?;
                let request = match target {
                    SelfConfigTarget::Tools => refuse_silent_tools_drops(
                        tools_request(&core, patch),
                        allow_drop,
                    ),
                    SelfConfigTarget::InferenceBackend => backend_request(patch),
                    _ => unreachable!("bound resource"),
                };
                self.patch(&core, verb, protect_working_agent(request))
                    .await
            }
            other => bail!(
                "unknown {resource} command {other:?}; accepted: get, preview, edit; see [\"help\",\"{resource}\"]"
            ),
        }
    }

    pub(super) async fn backend(&self, argv: &[String]) -> Result<String> {
        let verb = argv
            .first()
            .map(String::as_str)
            .context("backend command is required; see [\"help\",\"backend\"]")?;
        if verb == "list" {
            self.ensure_resource("backend")?;
            let parsed = ParsedArgs::parse(&argv[1..])?;
            parsed.reject_mutation_flags()?;
            return self
                .inference_inventory(
                    SelfConfigTarget::InferenceBackend,
                    parse_limit(&parsed)?,
                    parsed.one("cursor")?,
                )
                .await;
        }
        if verb == "accounts" {
            ParsedArgs::parse(&argv[1..])?.reject_mutation_flags()?;
            return self.account_inventory().await;
        }
        self.ensure_resource("backend")?;
        let create_args = match verb {
            "create" => Some(&argv[1..]),
            "preview" if argv.get(1).map(String::as_str) == Some("create") => Some(&argv[2..]),
            _ => None,
        };
        if let Some(create_args) = create_args {
            let parsed = ParsedArgs::parse(create_args)?;
            anyhow::ensure!(
                parsed.positionals.len() == 1,
                "backend create requires exactly one BACKEND_ID"
            );
            anyhow::ensure!(
                parsed.switches.is_empty(),
                "backend create accepts no switches"
            );
            for option in parsed.options.keys() {
                anyhow::ensure!(
                    matches!(option.as_str(), "endpoint" | "name" | "wire-api"),
                    "unknown backend create option --{option}; accepted: --endpoint, --name, --wire-api"
                );
            }
            let endpoint = parsed
                .one("endpoint")?
                .filter(|value| !value.trim().is_empty())
                .context("backend create requires --endpoint URL")?
                .to_owned();
            let wire_api =
                crate::openai_wire::OpenAiWireApi::parse_optional(parsed.one("wire-api")?)?;
            let request = local_backend_create_request(
                self.node_did.clone(),
                parsed.positionals[0].clone(),
                endpoint,
                parsed.one("name")?.map(ToOwned::to_owned),
                wire_api,
            );
            return self
                .patch(
                    &self.core,
                    if verb == "preview" { "preview" } else { "edit" },
                    request,
                )
                .await;
        }
        if verb == "discover" {
            anyhow::ensure!(argv.len() == 2, "backend discover requires one BACKEND_ID");
            let backend_id = &argv[1];
            let matches = crate::inference::backends::registry::list_enabled_backends_for_agent(
                &self.node,
                &self.node_did,
            )
            .await?
            .into_iter()
            .filter(|backend| backend.backend_id == *backend_id)
            .collect::<Vec<_>>();
            anyhow::ensure!(
                matches.len() == 1,
                "no unique enabled backend with ID {backend_id:?}; inspect config backend list"
            );
            let backend = &matches[0];
            if backend.provider_kind != crate::BackendProviderKind::OpenAiCompatible
                || !matches!(
                    backend.auth,
                    crate::document_config::BackendAuth::Unauthenticated
                )
            {
                let observation = self
                    .backend_observation_view(backend_id, backend.provider_kind)
                    .await?;
                return ordered! {
                    "backend_id": backend_id,
                    "refreshed": false,
                    "observation": observation,
                    "note": "This backend is credentialed, so self-config did not contact the provider. The catalog is the last credential-free observation published by operator discovery (desktop setup, gents config backend discover-models --backend-id) or the runtime prober; null means none has been recorded yet.",
                    "resource": "InferenceBackend",
                    "endpoint": backend.endpoint,
                    "provider_kind": backend.provider_kind,
                }
                .pretty();
            }
            self.execution.enter_mutation();
            let observation =
                crate::inference::backends::registry::discover_shared_backend(&self.node, backend)
                    .await?;
            let observation = crate::inference::backends::registry::scoped_observation_view(
                Some(&observation),
                &self.node_did,
                backend.provider_kind,
            )?;
            return ordered! {
                "backend_id": backend_id,
                "refreshed": true,
                "observation": observation,
                "note": "Provider-advertised facts were recorded for this persisted backend. Discovery did not create a profile, select a model, or change an agent.",
                "resource": "InferenceBackend",
                "endpoint": backend.endpoint,
            }
            .pretty();
        }
        self.bound_document("backend", argv).await
    }

    pub(super) async fn profile(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("profile")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("profile command is required; see [\"help\",\"profile\"]")?;
        if verb == "list" {
            let parsed = ParsedArgs::parse(&argv[1..])?;
            parsed.reject_mutation_flags()?;
            return self
                .inference_inventory(
                    SelfConfigTarget::InferenceProfile,
                    parse_limit(&parsed)?,
                    parsed.one("cursor")?,
                )
                .await;
        }
        let create_args = match verb {
            "create" => Some(&argv[1..]),
            "preview" if argv.get(1).map(String::as_str) == Some("create") => Some(&argv[2..]),
            _ => None,
        };
        if let Some(create_args) = create_args {
            let profile_id = create_args
                .first()
                .filter(|value| !value.starts_with("--"))
                .context(
                    "profile create requires target_id plus set.backend_id and set.model_name",
                )?;
            let patch = parse_patch(&create_args[1..], SelfConfigTarget::InferenceProfile)?;
            let fields = patch
                .iter()
                .filter_map(|(field, value)| value.as_ref().map(|_| field.as_str()))
                .collect::<BTreeSet<_>>();
            anyhow::ensure!(
                fields.contains("backend_id") && fields.contains("model_name"),
                "profile create requires --set backend_id=JSON and --set model_name=JSON"
            );
            let request = profile_create_request(self.node_did.clone(), profile_id.clone(), patch);
            return self
                .patch(
                    &self.core,
                    if verb == "preview" { "preview" } else { "edit" },
                    request,
                )
                .await;
        }
        let (agent_id, target_args) = extract_agent_target(&argv[1..])?;
        if agent_id.is_none() && matches!(verb, "preview" | "edit") {
            self.require_named_profile_owner(verb).await?;
        }
        let core = self.target_core(agent_id.as_deref(), "profile").await?;
        let mut rest = target_args.as_slice();
        let target_name = rest
            .first()
            .filter(|value| !value.starts_with("--"))
            .map(String::as_str)
            .unwrap_or("profile");
        if !rest.is_empty() && !rest[0].starts_with("--") {
            rest = &rest[1..];
        }
        if verb == "get"
            && !matches!(
                target_name,
                "profile" | "sampling" | "execution" | "retry-policy" | "compaction"
            )
        {
            anyhow::ensure!(rest.is_empty(), "profile get accepts one PROFILE_ID");
            return self
                .exact_read(SelfConfigTarget::InferenceProfile, target_name)
                .await;
        }
        let (request_name, target) = profile_target(target_name)?;
        match verb {
            "get" => {
                anyhow::ensure!(rest.is_empty(), "profile get accepts at most one target");
                self.bound_read(&core, target).await
            }
            "preview" | "edit" => {
                let patch = parse_patch(rest, target)?;
                self.patch(
                    &core,
                    verb,
                    protect_working_agent(profile_target_request(Some(request_name), patch)?),
                )
                .await
            }
            other => bail!(
                "unknown profile command {other:?}; accepted: get, preview, edit; see [\"help\",\"profile\"]"
            ),
        }
    }

    async fn require_named_profile_owner(&self, verb: &str) -> Result<()> {
        let owner = escape_graphql_string(&self.node_did);
        let query = format!("{{ Agent(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{agent_id}} }}");
        let response = crate::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.profile_owners",
            |txn| {
                let query = query.clone();
                Box::pin(async move { txn.execute(&query).await })
            },
        )
        .await?;
        let mut ids = gents_protocol::graphql::graphql_rows_from_response(&response, "Agent")
            .into_iter()
            .filter_map(|row| {
                row.get("agent_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect::<Vec<_>>();
        ids.sort();
        anyhow::ensure!(
            ids.len() <= 1,
            "profile {verb} needs options.agent because this node has several agents: {}. Name the agent whose profile to change; use {:?} for your own",
            ids.join(", "),
            self.core.agent_id()
        );
        Ok(())
    }
}

fn profile_target(name: &str) -> Result<(&'static str, SelfConfigTarget)> {
    match name {
        "profile" => Ok(("profile", SelfConfigTarget::InferenceProfile)),
        "sampling" => Ok(("sampling", SelfConfigTarget::InferenceSampling)),
        "execution" => Ok(("execution", SelfConfigTarget::InferenceExecution)),
        "retry-policy" => Ok(("retry_policy", SelfConfigTarget::InferenceRetryPolicy)),
        "compaction" => Ok(("compaction", SelfConfigTarget::Compaction)),
        other => bail!(
            "unknown profile target {other:?}; accepted: profile, sampling, execution, retry-policy, compaction"
        ),
    }
}
