use super::*;

impl ConfigCommandTool {
    pub(super) async fn schema(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("automation")?;
        self.core.identity()?;
        let access = crate::config_client::ConfigAccess::Local(self.node.clone());
        if argv.first().is_some_and(|arg| arg == "get") {
            anyhow::ensure!(argv.len() == 2, "schema get requires COLLECTION");
            return Ok(serde_json::to_string_pretty(
                &access
                    .collection_version(&argv[1])
                    .await?
                    .context("collection is not registered")?,
            )?);
        }
        let preview = argv.first().is_some_and(|arg| arg == "preview");
        let argv = if preview { &argv[1..] } else { argv };
        anyhow::ensure!(
            argv.first().is_some_and(|arg| arg == "install"),
            "use schema [preview] install --sdl SDL [--digest SHA256]; run config help schema"
        );
        let parsed = ParsedArgs::parse(&argv[1..])?;
        anyhow::ensure!(
            parsed.positionals.is_empty() && parsed.switches.is_empty(),
            "schema install accepts --sdl and --digest only"
        );
        for name in parsed.options.keys() {
            anyhow::ensure!(
                matches!(name.as_str(), "sdl" | "digest"),
                "unknown schema option --{name}"
            );
        }
        let sdl = parsed.one("sdl")?.context("--sdl SDL is required")?;
        anyhow::ensure!(
            sdl.len() <= 64 * 1024,
            "schema SDL exceeds 64 KiB; submit a smaller schema"
        );
        let digest = parsed.one("digest")?;
        let plan = if preview {
            anyhow::ensure!(self.dry_run, "schema preview is not granted");
            anyhow::ensure!(
                digest.is_none(),
                "preview returns the digest; do not supply --digest"
            );
            crate::config_client::preview_schema_install(&access, sdl).await?
        } else {
            crate::config_client::apply_schema_install(
                &access,
                sdl,
                digest.context("--digest from schema preview install is required")?,
            )
            .await?
        };
        Ok(serde_json::to_string_pretty(&json!({
            "plan": plan,
            "committed": !preview && plan.requires_publication,
            "verified": !preview,
            "scope": "Schema contracts are node-wide. Document reads/writes still require DefraDB ACP and explicit datastore tool selection. Schema publication is separate from configuration document transactions.",
        }))?)
    }
}
