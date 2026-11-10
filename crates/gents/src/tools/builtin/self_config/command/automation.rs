use super::*;
use crate::request::task_run;

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
                "unknown mcp-service command {other:?}; accepted: list, get, create, update, delete, preview, edit; see [\"help\",\"mcp-service\"]"
            ),
        }
    }

    /// `task run`: fire one Task now through the owner `gents automation task run` uses.
    /// The Task's agent answers, so another agent's Task needs the catalog grant.
    pub(super) async fn task_run(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("automation")?;
        anyhow::ensure!(
            argv.first().is_some_and(|verb| verb == "run"),
            "unknown task command; see [\"help\",\"task\"]"
        );
        let task_id = required_resource_id(argv.get(1), "TASK_ID")?;
        let parsed = ParsedArgs::parse(&argv[2..])?;
        anyhow::ensure!(
            parsed.positionals.is_empty()
                && parsed.switches.is_empty()
                && parsed
                    .options
                    .keys()
                    .all(|name| matches!(name.as_str(), "args" | "session-id" | "continue-session")),
            "task run takes TASK_ID and options args, session-id and continue-session; see [\"task\",\"run\",\"--help\"]"
        );
        let args = match parsed.one("args")? {
            Some(raw) => serde_json::from_str(raw).context("options.args must be a JSON object")?,
            None => json!({}),
        };
        let signer = self.core.agent_signer()?;
        let access = self.core.access()?;
        let task = task_run::read_runnable_task(&access, &self.node_did, task_id).await?;
        self.execution.enter_mutation();
        let output = task_run::enqueue_task_run(
            &access,
            &self.node_did,
            &signer,
            task,
            &task_run::TaskRunOptions {
                args,
                session_id: parsed.one("session-id")?.map(ToOwned::to_owned),
                continue_session: parsed.one("continue-session")?.map(ToOwned::to_owned),
            },
        )
        .await?;
        ordered! {
            "task_id": output.task_id,
            "agent_did": output.agent_did,
            "request_id": output.request_id,
            "session_id": output.session_id,
            "status": output.status,
            "duplicate": output.duplicate,
            "next": "The request is queued, not finished. Inspect its session with the sessions tool.",
        }
        .pretty()
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
                let (agent_did, rest) = extract_agent_target(&argv[3..])?;
                anyhow::ensure!(
                    rest.is_empty(),
                    "automation get accepts KIND, ID, and --agent AGENT_DID"
                );
                let core = self.target_core(agent_did.as_deref(), Some(target.category())).await?;
                self.automation_read(&core, target, id).await
            }
            "preview" | "edit" => {
                let (agent_did, rest) = extract_agent_target(&argv[3..])?;
                let core = self.target_core(agent_did.as_deref(), Some(target.category())).await?;
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
                    protect_working_agent(automation_request(&core, target, id.clone(), patch), false, self.holds_catalog_grant()),
                )
                .await
            }
            other => bail!(
                "unknown automation command {other:?}; accepted: get, preview, edit; see [\"help\",\"automation\"]"
            ),
        }
    }
}
