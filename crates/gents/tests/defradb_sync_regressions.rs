//! Regression fixtures at Gents' pinned DefraDB sync boundary.

use std::sync::Arc;

use db::merge::{BrowserSyncEngine, DbHeadProvider};
use db::{AutoCommitMutator, DB};
use document::Document;
use p2p::sync::DocumentHeadProvider;
use query::mutator::DocMutator;
use schema::{CollectionVersion, FieldDescription, FieldKind};
use storage::RegolithStore;

const COLLECTION: &str = "BrowserIngressFixture";
const COLLECTION_ID: &str = "browser-ingress-fixture-v1";

fn branchable_schema() -> CollectionVersion {
    CollectionVersion::new(
        COLLECTION,
        "browser-ingress-fixture-version-v1",
        COLLECTION_ID,
        vec![
            FieldDescription::new("1", "_docID", FieldKind::doc_id()),
            FieldDescription::new("2", "content", FieldKind::string()),
        ],
    )
    .as_branchable()
}

async fn branchable_db() -> Arc<DB<RegolithStore>> {
    let db = Arc::new(DB::new(RegolithStore::in_memory().expect("in-memory store")).expect("db"));
    db.create_collection(branchable_schema())
        .await
        .expect("branchable collection");
    db
}

/// A document accepted through `/sync` must join the collection DAG that
/// `sync_branchable_collection` uses for document-ID-free discovery.
#[tokio::test]
async fn browser_sync_ingress_is_discoverable_from_branchable_collection_heads() {
    let browser = branchable_db().await;
    let central = branchable_db().await;

    let mut document = Document::new();
    document.set("content", "created on the browser");
    let created = AutoCommitMutator::new(browser.clone())
        .create(COLLECTION, document)
        .await
        .expect("create browser document");

    let browser_sync = BrowserSyncEngine::new(browser);
    let document_ref = browser_sync
        .document_ref(&created.doc_id.to_string())
        .await
        .expect("load browser document reference")
        .expect("browser document reference");
    let pushed = browser_sync
        .load_document(&document_ref)
        .await
        .expect("load browser sync payload")
        .expect("browser sync payload");

    BrowserSyncEngine::new(central.clone())
        .apply_document(&pushed, "browser-fixture")
        .await
        .expect("apply browser sync payload");

    let heads = DbHeadProvider::new(central)
        .get_collection_heads(COLLECTION_ID)
        .await
        .expect("load branchable collection heads");
    assert_eq!(
        heads.len(),
        1,
        "the accepted document must have a collection commit so a late peer can discover it"
    );
}
