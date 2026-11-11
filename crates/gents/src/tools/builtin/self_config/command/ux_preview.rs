//! `config ux preview`: a page, rendered in the real sandbox on this node
//! (`ux_preview`), reported as text the model reads top to bottom. A read: it
//! writes only its report files. The page's data calls run as the calling
//! agent, so document ACP decides what the preview may show.

use super::*;

use crate::observe::ux_preview::{self as preview, PreviewRequest, Viewport};
use crate::plugin::{
    ux::{self, producer_input},
    PluginVerdict,
};

const HELP: &str = r#"ux preview: render a UX plugin's page in the real sandbox frame with this node's real data, at light and dark, and report what it shows.

  ux preview PLUGIN [--viewport WxH]

PLUGIN is a listing id (page/ID for a page document), a plugin's or page's name, or a path to a plugin.js inside your tool root (file tools granted); a path previews that file under the grants of the installed plugin with the same id.
The report names console errors, the visible text, elements clipped or past the viewport, CSS variables the sandbox does not define, low-contrast text, and each data call and its rows or error. It writes PNGs and report.json under the home.
Example: {"argv":["ux","preview","examples/board_pack/board"]}"#;

/// Whether this build can render a page at all.
pub(super) fn renders() -> bool {
    preview::renderer_available()
}

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
        // A name the home lists or a page document needs no file access. Any
        // other target is a draft file, read only inside the calling agent's
        // effective tool root and only with its file-read grant, as pack
        // authoring reads its files.
        let listed =
            ux::page::page_id(plugin).is_none() && preview::listing_named(home, plugin)?.is_some();
        let page = if listed {
            None
        } else if ux::page::page_id(plugin).is_some() || self.categories.contains("tools") {
            self.page_named(plugin).await?
        } else {
            None
        };
        let draft = if listed || page.is_some() {
            None
        } else {
            let file = self
                .authoring_paths("ux preview PATH", false)
                .await
                .and_then(|context| context.resolve_existing_file(plugin))
                .with_context(|| {
                    format!(
                        "no ux plugin or page {plugin:?}, and it is not a plugin.js this agent may read: a draft path is read inside your effective tool root with file tools granted. [\"ux\",\"list\"] names the installed plugins and pages"
                    )
                })?;
            Some(file.to_string_lossy().into_owned())
        };
        let target = match page {
            Some(page) => preview::PreviewTarget::page(&page)?,
            None => {
                preview::resolve_target(
                    home,
                    draft.as_deref().unwrap_or(plugin),
                    |namespace, name| async move {
                        let coordinate = format!("{namespace}/{name}");
                        let record = self.plugins.resolve(&coordinate, None)?;
                        let outcome = self.plugins.call_outcome(&record, producer_input()).await?;
                        anyhow::ensure!(
                            matches!(outcome.verdict, PluginVerdict::Success),
                            "the ux producer {coordinate} did not succeed: {}",
                            outcome.diagnostics
                        );
                        Ok(outcome.output)
                    },
                )
                .await?
            }
        };
        let slug = target
            .label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        let report = preview::run(
            PreviewRequest {
                home: home.to_owned(),
                access: self.core.access()?,
                node_did: self.node_did.clone(),
                out: home
                    .join(crate::store::local::home::UX_PREVIEW_DIR_NAME)
                    .join(slug),
                viewport,
            },
            target,
        )
        .await?;
        Ok(report.to_text(&json!({"argv": argv}).to_string()))
    }
}
