use super::*;
use crate::store::config_client::ConfigAccess;

impl ConfigCommandTool {
    /// `["trigger","explain",ID]` with optional `options.source-doc`. Read-only.
    pub(super) async fn trigger_explain(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("automation")?;
        anyhow::ensure!(
            argv.first().map(String::as_str) == Some("explain"),
            "trigger takes list, get, create, update, delete or explain; see [\"help\",\"trigger\"]"
        );
        let trigger_id = required_resource_id(argv.get(1), "TRIGGER_ID")?;
        let parsed = ParsedArgs::parse(&argv[2..])?;
        anyhow::ensure!(
            parsed.positionals.is_empty()
                && parsed.switches.is_empty()
                && parsed.options.keys().all(|key| key == "source-doc"),
            "trigger explain accepts TRIGGER_ID and options.source-doc"
        );
        let source_doc = parsed.one("source-doc")?;
        let owner = self.node_did.as_str();
        let explanation = ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.trigger_explain",
            |txn| {
                Box::pin(crate::documents::trigger_explain::explain_trigger_in_txn(
                    txn, owner, trigger_id, source_doc,
                ))
            },
        )
        .await?;
        Ok(serde_json::to_string_pretty(&explanation)?)
    }
}
