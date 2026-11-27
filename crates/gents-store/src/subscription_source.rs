use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use events::{DocumentChangeBatch, DocumentChangeSubscription, EventName, Subscription};

use crate::local::document_acp;

pub trait UpdateSubscriptionSource: Send + Sync {
    fn subscribe_updates(&self) -> Subscription;
    fn subscribe_changes(&self, collection_ids: Vec<String>) -> ChangeSubscription;
}

impl UpdateSubscriptionSource for EmbeddedNode {
    fn subscribe_updates(&self) -> Subscription {
        self.subscribe(&[EventName::Update])
    }

    fn subscribe_changes(&self, collection_ids: Vec<String>) -> ChangeSubscription {
        subscribe(self, collection_ids)
    }
}

impl UpdateSubscriptionSource for Arc<EmbeddedNode> {
    fn subscribe_updates(&self) -> Subscription {
        self.as_ref().subscribe(&[EventName::Update])
    }

    fn subscribe_changes(&self, collection_ids: Vec<String>) -> ChangeSubscription {
        subscribe(self, collection_ids)
    }
}

/// What a watcher's change subscription delivers (`AccessWake.Event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeEvent {
    RowsChanged,
    AccessChanged,
}

/// What a watcher reads again when an event arrives (`AccessWake.Delivery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    ChangedDocuments,
    WholeScope,
}

/// `AccessWake.delivery`.
pub fn delivery(event: ChangeEvent) -> Delivery {
    match event {
        ChangeEvent::RowsChanged => Delivery::ChangedDocuments,
        ChangeEvent::AccessChanged => Delivery::WholeScope,
    }
}

/// Changes in a watcher's collections and, on a home whose document access
/// control runs on an embedded Vera node, changes of the provider's access
/// state (`AccessWake`): a row whose access settles after its own change
/// event is read when it settles.
pub struct ChangeSubscription {
    rows: DocumentChangeSubscription,
    access: Option<Subscription>,
}

impl From<DocumentChangeSubscription> for ChangeSubscription {
    /// Row changes alone, as on a home on the node's own store.
    fn from(rows: DocumentChangeSubscription) -> Self {
        Self { rows, access: None }
    }
}

impl ChangeSubscription {
    /// `access` delivers the node's `AcpCacheInvalidated` events.
    pub fn new(rows: DocumentChangeSubscription, access: Option<Subscription>) -> Self {
        Self { rows, access }
    }

    /// The next batch of row changes, or an access change as a batch that
    /// asks for the whole scope. Every access change queued is taken with it:
    /// the read that follows sees all of them. Cancel-safe.
    pub async fn recv(&mut self) -> Option<DocumentChangeBatch> {
        loop {
            let Some(access) = self.access.as_mut() else {
                return self.rows.recv().await;
            };
            tokio::select! {
                batch = self.rows.recv() => return batch,
                message = access.recv() => match message {
                    Some(_) => {
                        while access.try_recv().is_ok() {}
                        return Some(access_batch());
                    }
                    None => self.access = None,
                },
            }
        }
    }
}

fn access_batch() -> DocumentChangeBatch {
    DocumentChangeBatch {
        changes: Vec::new(),
        resync_required: delivery(ChangeEvent::AccessChanged) == Delivery::WholeScope,
        updates: 0,
    }
}

/// Whether `batch` is an access change rather than row changes: it asks for
/// the whole scope and carries no update.
pub fn access_changed(batch: &DocumentChangeBatch) -> bool {
    batch.resync_required && batch.updates == 0 && batch.changes.is_empty()
}

/// Whether `batch` asks for the whole scope because more documents changed
/// than the subscription holds, rather than because access changed.
pub fn overflowed(batch: &DocumentChangeBatch) -> bool {
    batch.resync_required && !access_changed(batch)
}

/// Subscribe to changes in `collection_ids` and, where the node's provider
/// settles access after a row commits, to its access changes.
pub fn subscribe(node: &EmbeddedNode, collection_ids: Vec<String>) -> ChangeSubscription {
    let access = document_acp::access_wakes(document_acp::provider_of(node))
        .then(|| node.subscribe(&[EventName::AcpCacheInvalidated]));
    ChangeSubscription::new(node.subscribe_collection_changes(collection_ids), access)
}

/// Resolve installed collection names to the `collection_id`s that scoped
/// change subscriptions match. DefraDB keeps a collection's id stable across
/// schema patches, so a resolved scope never needs refreshing.
pub fn collection_ids<'a>(
    node: &EmbeddedNode,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<String>> {
    names
        .into_iter()
        .map(|name| {
            node.get_collection(name)?
                .map(|collection| collection.collection_id)
                .with_context(|| format!("{name} schema is not installed"))
        })
        .collect()
}

/// Subscribe to coalesced changes in the named collections only. Subscribe
/// before the initial read so a change committed in between stays pending.
pub fn subscribe_collections<'a>(
    node: &EmbeddedNode,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<ChangeSubscription> {
    Ok(subscribe(node, collection_ids(node, names)?))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Clone, Copy, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Event {
        RowsChanged,
        AccessChanged,
    }

    #[derive(Debug, Clone, Copy, serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    enum Scope {
        ChangedDocuments,
        WholeScope,
    }

    #[test]
    fn delivery_matches_the_lean_model() {
        gents_lean_vocab::families::access_wake_delivery::replay::<Scope, Event>(|input| {
            let event = match input.event {
                Event::RowsChanged => ChangeEvent::RowsChanged,
                Event::AccessChanged => ChangeEvent::AccessChanged,
            };
            match delivery(event) {
                Delivery::ChangedDocuments => Scope::ChangedDocuments,
                Delivery::WholeScope => Scope::WholeScope,
            }
        });
    }

    fn cache_invalidated() -> events::Message {
        events::Message::acp_cache_invalidated(events::AcpCacheInvalidatedData {
            height: 2,
            module_state_root: "new".into(),
            previous_root: "old".into(),
            entries_invalidated: 1,
        })
    }

    /// TLA `AccessWake`: an access change wakes the watcher once for all the
    /// changes queued before it reads, as a batch asking for its whole scope,
    /// and row changes still arrive as row changes.
    #[tokio::test]
    async fn access_changes_wake_a_subscription_that_listens_for_them() {
        use events::Bus as _;
        let bus = events::ChannelBus::new();
        let mut listening = ChangeSubscription::new(
            bus.subscribe_collection_changes(vec!["watched".into()]),
            Some(bus.subscribe(&[EventName::AcpCacheInvalidated])),
        );
        let mut rows_only =
            ChangeSubscription::from(bus.subscribe_collection_changes(vec!["watched".into()]));
        bus.publish(cache_invalidated());
        bus.publish(cache_invalidated());

        let batch = tokio::time::timeout(Duration::from_secs(5), listening.recv())
            .await
            .expect("an access change wakes a listening subscription")
            .expect("subscription open");
        assert!(access_changed(&batch), "{batch:?}");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listening.recv())
                .await
                .is_err(),
            "queued access changes wake it once"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rows_only.recv())
                .await
                .is_err(),
            "a subscription on the node's own store is not woken by access changes"
        );

        let block = b"watched:doc".to_vec();
        let cid = defra_core::block::generate_cid_from_bytes(&block).unwrap();
        bus.publish(events::Message::update(events::Update::new(
            "doc".to_owned(),
            cid,
            "watched".to_owned(),
            block,
            false,
            true,
        )));
        for subscription in [&mut listening, &mut rows_only] {
            let batch = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
                .await
                .expect("a row change wakes the subscription")
                .expect("subscription open");
            assert!(!access_changed(&batch), "{batch:?}");
            assert_eq!(batch.changes.len(), 1);
        }
    }

    #[tokio::test]
    async fn scoped_changes_survive_a_schema_patch_and_ignore_other_collections() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_collection(
            &node.node_context(),
            "type Watched { value: String } type Unwatched { value: String }",
        )
        .await
        .unwrap();
        assert!(collection_ids(&node, ["Missing"]).is_err());
        let watched = collection_ids(&node, ["Watched"]).unwrap();
        let mut changes = subscribe_collections(&node, ["Watched"]).unwrap();
        node.patch_collection_as(
            &defra_core::RequestContext::default(),
            r#"[{"op":"add","path":"/Watched/Fields/-","value":{"Name":"extra","Kind":"String"}}]"#,
            None,
        )
        .await
        .unwrap();
        assert_eq!(collection_ids(&node, ["Watched"]).unwrap(), watched);

        let unwatched = node
            .execute(r#"mutation { create_Unwatched(input: { value: "x" }) { _docID } }"#)
            .await;
        assert!(!unwatched.has_errors(), "{:?}", unwatched.errors);
        let created = node
            .execute(r#"mutation { create_Watched(input: { value: "y", extra: "z" }) { _docID } }"#)
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);

        let batch = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("watched write wakes the subscription")
            .expect("subscription open");
        assert!(!batch.resync_required);
        assert!(!batch.changes.is_empty());
        assert!(batch
            .changes
            .iter()
            .all(|change| change.collection_id == watched[0]));
    }
}
