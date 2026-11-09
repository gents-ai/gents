use super::*;
use crate::plugin::ux;

/// The generated SDK reference `help page sdk` returns.
pub(super) const SDK_REFERENCE: &str = ux::reference::SDK_REFERENCE;

/// How much module source one tool result carries; the digest and byte count
/// describe the rest.
const MODULE_SOURCE_LIMIT: usize = 64 * 1024;

impl ConfigCommandTool {
    pub(super) async fn ux(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.core.held_grants().pack_install,
            "ux inspection requires the pack grant"
        );
        match argv.first().map(String::as_str) {
            Some("list") => self.ux_list(&argv[1..]),
            Some("module") => self.ux_module(&argv[1..]).await,
            Some("lint") => self.ux_lint(&argv[1..]).await,
            _ => bail!("ux supports list, module and lint; see [\"help\",\"page\"]"),
        }
    }

    fn ux_list(&self, argv: &[String]) -> Result<String> {
        let home = self
            .plugins
            .home()
            .context("this runtime has no plugin home")?;
        let parsed = ParsedArgs::parse(argv)?;
        parsed.reject_mutation_flags()?;
        let limit = parse_limit(&parsed)?;
        let cursor = parsed.one("cursor")?;
        let mut items = ux::list(home)?
            .into_iter()
            .filter(|listing| cursor.is_none_or(|cursor| listing.id.as_str() > cursor))
            .take(limit + 1)
            .collect::<Vec<_>>();
        let truncated = items.len() > limit;
        items.truncate(limit);
        let next_cursor = truncated
            .then(|| items.last().map(|listing| listing.id.clone()))
            .flatten();
        ordered! {
            "items": items,
            "next_cursor": next_cursor,
            "next_call": json!({"argv":["ux","module","<ID>"]}),
        }
        .pretty()
    }

    async fn ux_module(&self, argv: &[String]) -> Result<String> {
        let home = self
            .plugins
            .home()
            .context("this runtime has no plugin home")?;
        let [id] = argv else {
            bail!("ux module requires exactly one listed ID; see [\"ux\",\"list\"]");
        };
        let listing = ux::find(home, id)?;
        let plugins = self.plugins.clone();
        let module = ux::resolve_module(home, &listing, |namespace, plugin| async move {
            let coordinate = format!("{namespace}/{plugin}");
            let record = plugins.resolve(&coordinate, None)?;
            plugins
                .call(&record, ux::producer_input())
                .await?
                .outcome
                .into_output(&coordinate)
        })
        .await?;
        let bytes = module.source.len();
        let source: String = module
            .source
            .char_indices()
            .take_while(|(index, _)| *index < MODULE_SOURCE_LIMIT)
            .map(|(_, ch)| ch)
            .collect();
        ordered! {
            "id": id,
            "digest": module.digest,
            "bytes": bytes,
            "truncated": bytes > source.len(),
            "source": source,
            "css": module.css,
            "next_call": json!({"argv":["help","page"]}),
        }
        .pretty()
    }

    async fn ux_lint(&self, argv: &[String]) -> Result<String> {
        let [file] = argv else {
            bail!("ux lint requires exactly one FILE inside the tool root");
        };
        let context = self.authoring_paths("ux lint", false).await?;
        let path = context.resolve_existing_file(file)?;
        let display = context.display_path(&path);
        let findings = tokio::task::spawn_blocking(move || ux::lint_file(&path))
            .await
            .context("ux lint task did not finish")??;
        ordered! {
            "file": display,
            "admitted": findings.is_empty(),
            "findings": findings,
            "next_call": if findings.is_empty() { json!({"argv":["help","page"]}) } else { json!({"argv":["help","page","contributions"]}) },
        }
        .pretty()
    }
}
