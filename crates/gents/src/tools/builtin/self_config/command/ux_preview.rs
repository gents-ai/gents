//! `config ux preview`: a pack's UI page, rendered in the real sandbox on
//! this node (`ux_preview`), reported as text the model
//! reads top to bottom. A read: it writes only its report files.

use super::*;

use crate::observe::ux_preview::{self as preview, PreviewRequest, Viewport};
use crate::plugin::{ux::producer_input, PluginVerdict};

const HELP: &str = r#"ux preview: render a UX plugin's page in the real sandbox frame with this node's real data, at light and dark, and report what it shows.

  ux preview PLUGIN [--viewport WxH]

PLUGIN is a listing id, a plugin's name, or a path to a plugin.js; a path previews that file under the grants of the installed plugin with the same id.
The report names console errors, the visible text, elements clipped or past the viewport, CSS variables the sandbox does not define, low-contrast text, and each data call and its rows or error. It writes PNGs and report.json under the home.
Example: {"argv":["ux","preview","examples/board_pack/board"]}"#;

pub(super) fn is_ux_preview(argv: &[String]) -> bool {
    matches!(argv, [resource, verb, ..] if resource == "ux" && verb == "preview")
}

impl ConfigCommandTool {
    pub(super) async fn ux_preview(&self, argv: &[String]) -> Result<String> {
        if argv
            .iter()
            .any(|word| matches!(word.as_str(), "--help" | "-h"))
        {
            return Ok(HELP.to_owned());
        }
        anyhow::ensure!(
            self.core.held_grants().pack_install || self.categories.contains("tools"),
            "ux preview requires the pack or tools grant"
        );
        let parsed = ParsedArgs::parse(&argv[2..])?;
        anyhow::ensure!(
            parsed.switches.is_empty() && parsed.options.keys().all(|name| name == "viewport"),
            "ux preview accepts PLUGIN and options.viewport only; see [\"ux\",\"preview\",\"--help\"]"
        );
        let [plugin] = parsed.positionals.as_slice() else {
            bail!("ux preview needs one PLUGIN: a listing id, a plugin's name or a plugin.js path; see [\"ux\",\"preview\",\"--help\"]");
        };
        let viewport = parsed
            .one("viewport")?
            .map(str::parse::<Viewport>)
            .transpose()?
            .unwrap_or_default();
        let home = self
            .plugins
            .home()
            .context("this runtime has no plugin home")?;
        let target = preview::resolve_target(home, plugin, |namespace, name| async move {
            let coordinate = format!("{namespace}/{name}");
            let record = self.plugins.resolve(&coordinate, None)?;
            let call = self.plugins.call(&record, producer_input()).await?;
            anyhow::ensure!(
                matches!(call.outcome.verdict, PluginVerdict::Success),
                "the ux producer {coordinate} did not succeed: {}",
                call.outcome.diagnostics
            );
            Ok(call.outcome.output)
        })
        .await?;
        let slug = target
            .label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        let report = preview::run(
            PreviewRequest {
                home: home.to_owned(),
                access: crate::store::config_client::ConfigAccess::local(self.node.clone()),
                node_did: self.node_did.clone(),
                out: home.join("ux-preview").join(slug),
                viewport,
            },
            target,
        )
        .await?;
        Ok(report.to_text(&json!({"argv": argv}).to_string()))
    }
}
