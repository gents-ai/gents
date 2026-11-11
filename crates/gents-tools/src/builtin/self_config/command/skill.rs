use super::*;
use gents_documents::operations::skill::{SkillImport, SkillSourceRoot};
use gents_protocol::operations::skill::SkillOperationError;

impl ConfigCommandTool {
    pub(super) async fn skill(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        let preview = argv.first().is_some_and(|arg| arg == "preview");
        let argv = if preview { &argv[1..] } else { argv };
        if argv.first().is_some_and(|arg| arg == "get") && !preview {
            anyhow::ensure!(argv.len() == 2, "skill get requires SKILL_ID");
            return self.exact_read(SelfConfigTarget::Skill, &argv[1]).await;
        }
        const USAGE: &str = "use config skill [preview] import SKILL_ID PATH; PATH is a directory or SKILL.md file; options.overwrite = true replaces a stored skill";
        anyhow::ensure!(argv.first().is_some_and(|arg| arg == "import"), USAGE);
        let parsed = ParsedArgs::parse(&argv[1..])?;
        anyhow::ensure!(
            parsed.positionals.len() == 2
                && parsed.switches.is_empty()
                && parsed.options.keys().all(|name| name == "overwrite"),
            USAGE
        );
        let overwrite = match parsed.one("overwrite")? {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => bail!("options.overwrite is true or false; got {other:?}"),
        };
        if preview {
            anyhow::ensure!(self.preview, "preview is not granted for this agent");
        }
        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let mode = effective
            .pointer("/runtime_effective/effective/file_mode")
            .and_then(Value::as_str)
            .map(gents_documents::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        if mode == gents_documents::FileToolMode::Off {
            return Err(self.skill_refused(SkillOperationError::FileReadNotGranted));
        }
        let Some(root) = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
        else {
            return Err(self.skill_refused(SkillOperationError::ToolRootMissing));
        };
        let context = crate::native::ToolContext::new(root.into(), false)?;
        let resolve = |path: &std::path::Path| {
            context.resolve_path(path.to_str().context("skill path must be UTF-8")?)
        };
        let import = SkillImport {
            owner: &self.node_did,
            skill_id: &parsed.positionals[0],
            source: std::path::Path::new(&parsed.positionals[1]),
            root: SkillSourceRoot::ToolRoot(&resolve),
            overwrite,
            enabled: true,
            node_binding: self.core.node_binding(),
        };
        if !preview {
            self.execution.enter_mutation();
        }
        let receipt = gents_documents::operations::skill::import(
            &self.core.access()?,
            &import,
            &gents_inference::resolved::ProviderProfilePolicy,
            !preview,
        )
        .await
        .map_err(|failure| match failure {
            gents_documents::operations::OperationFailure::Operation(error) => {
                self.skill_refused(error)
            }
            gents_documents::operations::OperationFailure::Other(error) => error,
        })?;
        outcome_text(&PatchOutcome::from_receipt(
            SelfConfigTarget::Skill,
            self.core.agent_did(),
            receipt,
        ))
    }

    /// A refused skill operation as the model reads it. An overwrite is
    /// spelled as the option that grants it; another owner's failure passes
    /// on unchanged to that owner's classifiers.
    fn skill_refused(&self, error: SkillOperationError) -> anyhow::Error {
        let next_call = match &error {
            SkillOperationError::ImportExists { skill_id } => {
                json!({"argv":["skill","preview","import",skill_id,"<PATH>"],"options":{"overwrite":true}})
            }
            other => guidance::next_call(other.recovery()),
        };
        CommandGuidance {
            message: gents_protocol::error_chain::ErrorChain(&error).to_string(),
            next_call,
        }
        .into()
    }
}
