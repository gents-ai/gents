//! Pages held on the node as `Page` documents. Their create, read, update and
//! delete are the shared document verbs; this file adds what is specific to a
//! page: the lint, and the listing and module the `ux` verbs and the desktop
//! read. Every read here is the calling agent's own, under document ACP.

use super::*;

use crate::documents::PageDocument;
use ux::{page as ux_page, UxListing};

impl ConfigCommandTool {
    pub(in super::super) async fn page(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        match argv {
            [verb, id] if verb == "lint" => self.page_lint(id).await,
            _ => bail!(
                "page supports list, get, create, update, delete and lint ID; see [\"help\",\"page\"]"
            ),
        }
    }

    /// The page document `id` on this node, when the calling agent may read
    /// one by that id.
    async fn page_document_if_any(&self, id: &str) -> Result<Option<PageDocument>> {
        let owner = self.node_did.clone();
        let lookup = id.to_owned();
        let target = SelfConfigTarget::Page;
        crate::store::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.page_read",
            |txn| {
                let owner = owner.clone();
                let lookup = lookup.clone();
                Box::pin(async move { ops::read_owned_doc(txn, target, &owner, &lookup).await })
            },
        )
        .await?
        .map(|(_, document)| {
            serde_json::from_value(Value::Object(document)).context("decoding the page document")
        })
        .transpose()
    }

    /// The page document `id` on this node, as the calling agent may read it.
    pub(in super::super) async fn page_document(&self, id: &str) -> Result<PageDocument> {
        Ok(self
            .page_document_if_any(id)
            .await?
            .ok_or(super::ops::MissingConfigDocument {
                target: SelfConfigTarget::Page,
                id: id.to_owned(),
            })?)
    }

    /// The page document a `ux` verb's argument names: `page/ID`, which must
    /// exist, or a bare ID, which may name one.
    pub(in super::super) async fn page_named(&self, name: &str) -> Result<Option<PageDocument>> {
        match ux_page::page_id(name) {
            Some(id) => self.page_document(id).await.map(Some),
            None if crate::documents::is_snake_case_name(name) => {
                self.page_document_if_any(name).await
            }
            None => Ok(None),
        }
    }

    /// Every page document on this node the calling agent may read.
    pub(in super::super) async fn page_listings(&self) -> Result<Vec<UxListing>> {
        ux_page::list(&self.core.access()?, &self.node_did).await
    }

    pub(in super::super) async fn page_lint(&self, id: &str) -> Result<String> {
        let id = ux_page::page_id(id).unwrap_or(id);
        let page = self.page_document(id).await?;
        let findings = ux_page::lint(&page);
        let invalid = page.validate().err().map(|error| format!("{error:#}"));
        let admitted = findings.is_empty() && invalid.is_none();
        let listing = ux_page::listing_id(&page.page_id);
        let renders = crate::observe::ux_preview::renderer_available();
        ordered! {
            "page": page.page_id,
            "admitted": admitted,
            "invalid": invalid,
            "findings": findings,
            "enabled": page.enabled,
            "next_call": if !admitted {
                Some(json!({"argv":["help","page","contributions"]}))
            } else if renders {
                Some(json!({"argv":["ux","preview",listing]}))
            } else {
                None
            },
            "note": (admitted && !renders).then_some("Lint is the check this build has: it has no page renderer, so ux preview renders nothing here. The page is seen in the desktop app."),
        }
        .pretty()
    }
}
