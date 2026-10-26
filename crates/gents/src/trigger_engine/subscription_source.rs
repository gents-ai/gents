use std::sync::Arc;

use defra_node::EmbeddedNode;
use events::{DocumentChangeSubscription, EventName, Subscription};

#[derive(Debug, thiserror::Error)]
pub enum SubscriptionSourceError {
    #[error(transparent)]
    Node(Box<defra_node::Error>),
    #[error("{name} schema is not installed")]
    NotInstalled { name: String },
}

type Result<T, E = SubscriptionSourceError> = std::result::Result<T, E>;

pub trait UpdateSubscriptionSource: Send + Sync {
    fn subscribe_updates(&self) -> Subscription;
    fn subscribe_collection_changes(
        &self,
        collection_ids: Vec<String>,
    ) -> DocumentChangeSubscription;
}

impl UpdateSubscriptionSource for EmbeddedNode {
    fn subscribe_updates(&self) -> Subscription {
        self.subscribe(&[EventName::Update])
    }

    fn subscribe_collection_changes(
        &self,
        collection_ids: Vec<String>,
    ) -> DocumentChangeSubscription {
        EmbeddedNode::subscribe_collection_changes(self, collection_ids)
    }
}

impl UpdateSubscriptionSource for Arc<EmbeddedNode> {
    fn subscribe_updates(&self) -> Subscription {
        self.as_ref().subscribe(&[EventName::Update])
    }

    fn subscribe_collection_changes(
        &self,
        collection_ids: Vec<String>,
    ) -> DocumentChangeSubscription {
        self.as_ref().subscribe_collection_changes(collection_ids)
    }
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
            node.get_collection(name)
                .map_err(|error| SubscriptionSourceError::Node(Box::new(error)))?
                .map(|collection| collection.collection_id)
                .ok_or_else(|| SubscriptionSourceError::NotInstalled {
                    name: name.to_owned(),
                })
        })
        .collect()
}

/// Subscribe to coalesced changes in the named collections only. Subscribe
/// before the initial read so a change committed in between stays pending.
pub fn subscribe_collections<'a>(
    node: &EmbeddedNode,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<DocumentChangeSubscription> {
    Ok(node.subscribe_collection_changes(collection_ids(node, names)?))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn scoped_changes_survive_a_schema_patch_and_ignore_other_collections() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema("type Watched { value: String } type Unwatched { value: String }")
            .await
            .unwrap();
        assert!(collection_ids(&node, ["Missing"]).is_err());
        let watched = collection_ids(&node, ["Watched"]).unwrap();
        let mut changes = subscribe_collections(&node, ["Watched"]).unwrap();
        node.patch_collection(
            "Watched",
            r#"[{"op":"add","path":"/Watched/Fields/-","value":{"Name":"extra","Kind":"String"}}]"#,
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
