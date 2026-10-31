use super::*;

impl ConfigCommandTool {
    pub(super) async fn mcp_service(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("mcp-service")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("mcp-service command is required; see [\"help\",\"mcp-service\"]")?;
        let id = required_resource_id(argv.get(1), "SERVICE_ID")?;
        match verb {
            "get" => {
                anyhow::ensure!(argv.len() == 2, "mcp-service get accepts one SERVICE_ID");
                self.exact_read(SelfConfigTarget::ToolServiceRegistry, id)
                    .await
            }
            "preview" | "edit" => {
                let patch = parse_patch(&argv[2..], SelfConfigTarget::ToolServiceRegistry)?;
                self.patch(&self.core, verb, mcp_service_request(id.clone(), patch))
                    .await
            }
            other => bail!(
                "unknown mcp-service command {other:?}; accepted: get, preview, edit; see [\"help\",\"mcp-service\"]"
            ),
        }
    }

    pub(super) async fn automation(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("automation")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("automation command is required; see [\"help\",\"automation\"]")?;
        if verb == "list" {
            let kind = argv
                .get(1)
                .context("automation list requires task, trigger, schedule or event-source")?;
            let target = automation_target(&kind.replace('-', "_"))?;
            let parsed = ParsedArgs::parse(&argv[2..])?;
            parsed.reject_mutation_flags()?;
            return self
                .inference_inventory(target, parse_limit(&parsed)?, parsed.one("cursor")?)
                .await;
        }
        let kind = argv.get(1).context("automation requires KIND")?;
        let id = required_resource_id(argv.get(2), "automation ID")?;
        let target = automation_target(&kind.replace('-', "_"))?;
        match verb {
            "get" => {
                let (agent_id, rest) = extract_agent_target(&argv[3..])?;
                anyhow::ensure!(
                    rest.is_empty(),
                    "automation get accepts KIND, ID, and --agent AGENT_ID"
                );
                let core = self.target_core(agent_id.as_deref(), "automation").await?;
                self.automation_read(&core, target, id).await
            }
            "preview" | "edit" => {
                let (agent_id, rest) = extract_agent_target(&argv[3..])?;
                let core = self.target_core(agent_id.as_deref(), "automation").await?;
                let patch = parse_patch(&rest, target)?;
                anyhow::ensure!(
                    target != SelfConfigTarget::EventSource
                        || patch.iter().all(|(field, value)| {
                            field != "filter"
                                || value.as_ref().is_none_or(|value| value.is_string() || value.is_null())
                        }),
                    "filter must be a string holding a GraphQL object literal with unquoted keys, e.g. \"{{kind: {{_eq: \\\"review\\\"}}}}\", not a JSON object; see [\"help\",\"automation\"]"
                );
                self.patch(
                    &core,
                    verb,
                    protect_working_agent(automation_request(&core, target, id.clone(), patch)),
                )
                .await
            }
            other => bail!(
                "unknown automation command {other:?}; accepted: get, preview, edit; see [\"help\",\"automation\"]"
            ),
        }
    }
}
