use super::*;

impl ConfigCommandTool {
    pub(super) async fn skill(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        let preview = argv.first().is_some_and(|arg| arg == "preview");
        let argv = if preview { &argv[1..] } else { argv };
        if argv.first().is_some_and(|arg| arg == "get") && !preview {
            anyhow::ensure!(argv.len() == 2, "skill get requires SKILL_ID");
            return self.exact_read(SelfConfigTarget::Skill, &argv[1]).await;
        }
        anyhow::ensure!(
            argv.len() == 3 && argv[0] == "import",
            "use config skill [preview] import SKILL_ID PATH; PATH is a directory or SKILL.md file"
        );
        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let mode = effective
            .pointer("/runtime_effective/effective/file_mode")
            .and_then(Value::as_str)
            .map(crate::tool_surface::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        anyhow::ensure!(
            mode != crate::tool_surface::FileToolMode::Off,
            "skill import requires effective file read permission on the invoking behavior"
        );
        let root = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
            .context("skill import requires an effective tool root")?;
        let context = crate::toolset::ToolContext::new(root.into(), false)?;
        let document = crate::skills::import::load_skill_source(
            std::path::Path::new(&argv[2]),
            &argv[1],
            &self.agent_did,
            |path| context.resolve_path(path.to_str().context("skill path must be UTF-8")?),
        )?;
        let target = SelfConfigTarget::Skill;
        let value = serde_json::to_value(document)?;
        let patch = target
            .writable_fields()
            .into_iter()
            .filter_map(|field| {
                value
                    .get(field)
                    .map(|v| (field.to_owned(), Some(v.clone())))
            })
            .collect();
        let mut request = ApplyRequest::new(target, patch);
        let skill_id = argv[1].clone();
        request.resolve_unique = Box::new(move |_| Ok(skill_id.clone()));
        // Imports create an explicit new identity. Never overwrite an existing
        // skill that may be referenced by Setup or another working behavior.
        request.allow_create = true;
        request.require_create = true;
        let owner = self.agent_did.clone();
        request.on_create = Box::new(move |id, doc| {
            doc.insert("skill_id".into(), json!(id));
            doc.insert("agent_did".into(), json!(owner));
            Ok(())
        });
        self.patch(
            &self.core,
            if preview { "preview" } else { "edit" },
            request,
        )
        .await
    }
}
