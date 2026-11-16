//! The document door: a page held on the node as a `Page` document
//! (`crate::documents::PageDocument`), listed and produced from the node's
//! documents rather than from a home record or a folder. One page is one
//! listing, `page/<page_id>`: a dev listing has no slash and a pack listing
//! has two, so the three doors never share an id.
//!
//! The reader's own access decides what it sees: document ACP on the node,
//! with no fallback to the node's identity.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::{digest_of, UxContributions, UxDoor, UxListing, UxModule, UxProducer};
use gents_documents::{is_snake_case_name, list_pages, PageDocument, PAGE_AREAS};
use gents_store::config_client::ConfigAccess;

/// What a document listing's id starts with.
pub const DOCUMENT_ID_PREFIX: &str = "page/";

/// The `page_id` a listing id names, when it is a document listing.
pub fn page_id(listing_id: &str) -> Option<&str> {
    listing_id
        .strip_prefix(DOCUMENT_ID_PREFIX)
        .filter(|id| is_snake_case_name(id))
}

/// What every page document may register (`PAGE_AREAS`): the gate the
/// desktop's plugin context enforces on each `register` call.
pub fn contributes() -> UxContributions {
    UxContributions {
        areas: PAGE_AREAS.iter().map(|area| (*area).to_owned()).collect(),
        directives: Vec::new(),
    }
}

/// The listing id of the page document `page_id`.
pub fn listing_id(page_id: &str) -> String {
    format!("{DOCUMENT_ID_PREFIX}{page_id}")
}

/// What the desktop compares to learn a page changed: the module, the
/// stylesheet and the data grants.
fn content_digest(page: &PageDocument) -> Result<String> {
    let grants = serde_json::to_vec(page.data())?;
    let mut hasher = Sha256::new();
    for part in [
        page.module.as_bytes(),
        page.css.as_deref().unwrap_or_default().as_bytes(),
        grants.as_slice(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// The listing of one page document. `default_enabled` carries the
/// document's `enabled`: the document, not a per-window choice, decides.
pub fn listing(page: &PageDocument) -> Result<UxListing> {
    Ok(UxListing {
        id: listing_id(&page.page_id),
        door: UxDoor::Document,
        producer: UxProducer::File,
        file: format!("Page/{}", page.page_id),
        pack: None,
        description: page
            .description
            .clone()
            .or_else(|| page.display_name.clone()),
        contributes: Some(contributes()),
        default_enabled: Some(page.enabled),
        data: page.data().to_vec(),
        digest: Some(content_digest(page)?),
    })
}

/// The module the desktop evaluates for one page document.
pub fn module(page: &PageDocument) -> UxModule {
    UxModule {
        digest: digest_of(&page.module),
        source: page.module.clone(),
        css: page.css.clone().filter(|css| !css.trim().is_empty()),
    }
}

/// Every page document of `node_did` the reader may read, as listings.
pub async fn list(access: &ConfigAccess, node_did: &str) -> Result<Vec<UxListing>> {
    list_pages(access, node_did)
        .await?
        .iter()
        .map(listing)
        .collect()
}

/// The page document a document listing id names on `node_did`.
pub async fn find(access: &ConfigAccess, node_did: &str, listing_id: &str) -> Result<PageDocument> {
    let id = page_id(listing_id)
        .with_context(|| format!("{listing_id:?} is not a page document listing"))?;
    list_pages(access, node_did)
        .await?
        .into_iter()
        .find(|page| page.page_id == id)
        .with_context(|| format!("no page {id:?} on node {node_did}"))
}

/// Lints a page document as the desktop would receive it.
pub fn lint(page: &PageDocument) -> Vec<super::lint::Finding> {
    super::lint::lint_plugin(&page.module, page.css.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(module: &str) -> PageDocument {
        serde_json::from_value(serde_json::json!({
            "page_id": "board", "node_did": "did:key:n", "module": module,
            "data": [{"surface": "board", "tools": ["list_cards"]}],
        }))
        .unwrap()
    }

    #[test]
    fn a_document_listing_is_keyed_apart_from_the_other_doors() {
        let listing = listing(&page("export default {}")).unwrap();
        assert_eq!(listing.id, "page/board");
        assert_eq!(listing.door, UxDoor::Document);
        assert_eq!(page_id(&listing.id), Some("board"));
        assert_eq!(listing.data[0].tools, ["list_cards"]);
        for other in ["board", "acme/kit/board", "page/Not Snake", "page/"] {
            assert_eq!(page_id(other), None, "{other}");
        }
    }

    #[test]
    fn an_edit_changes_the_listing_digest() {
        let before = listing(&page("export default {}")).unwrap().digest;
        let after = listing(&page("export default { id: 'board' }"))
            .unwrap()
            .digest;
        assert_ne!(before, after);
        let mut restyled = page("export default {}");
        restyled.css = Some(".a{}".into());
        assert_ne!(before, listing(&restyled).unwrap().digest);
    }
}
