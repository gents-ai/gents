pub enum SnapshotChanges {
    Local(crate::subscription_source::ChangeSubscription),
    Remote {
        access: Box<crate::config_client::ConfigAccess>,
        query: String,
        changes: crate::config_client::GraphqlSubscription,
    },
}

impl SnapshotChanges {
    pub async fn recv(&mut self) {
        match self {
            Self::Local(changes) => {
                changes.recv().await.expect("document subscription closed");
            }
            Self::Remote {
                access,
                query,
                changes,
            } => {
                if changes
                    .recv()
                    .await
                    .expect("document subscription failed")
                    .is_none()
                {
                    assert_eq!(
                        changes.end(),
                        Some(crate::config_client::SubscriptionEnd::RenewalDue),
                        "document subscription closed unexpectedly"
                    );
                    *changes = access
                        .subscribe(query)
                        .await
                        .expect("renew document subscription");
                }
            }
        }
    }
}

pub trait SnapshotReader {
    fn subscribe_changes(
        &self,
        collection: &str,
    ) -> impl std::future::Future<Output = SnapshotChanges>;
    fn read_data(&self, query: &str) -> impl std::future::Future<Output = serde_json::Value>;
}

impl SnapshotReader for defra_node::Node {
    async fn subscribe_changes(&self, collection: &str) -> SnapshotChanges {
        SnapshotChanges::Local(
            crate::subscription_source::subscribe_collections(self, [collection])
                .expect("observe collection"),
        )
    }
    async fn read_data(&self, query: &str) -> serde_json::Value {
        let response = crate::graphql::graphql_with_transaction_retry(self, query, "test.snapshot")
            .await
            .expect("snapshot read");
        assert!(
            !response.has_errors(),
            "snapshot query failed: {:?}",
            response.errors
        );
        response.data.unwrap_or_default()
    }
}

impl<T> SnapshotReader for std::sync::Arc<T>
where
    T: SnapshotReader + ?Sized,
{
    async fn subscribe_changes(&self, collection: &str) -> SnapshotChanges {
        self.as_ref().subscribe_changes(collection).await
    }
    async fn read_data(&self, query: &str) -> serde_json::Value {
        self.as_ref().read_data(query).await
    }
}

impl SnapshotReader for crate::config_client::ConfigAccess {
    async fn subscribe_changes(&self, collection: &str) -> SnapshotChanges {
        if let Some(node) = self.local_node() {
            node.subscribe_changes(collection).await
        } else {
            let query = format!("subscription {{ {collection} {{ _docID }} }}");
            let changes = self.subscribe(&query).await.expect("subscribe as actor");
            SnapshotChanges::Remote {
                access: Box::new(self.clone()),
                query,
                changes,
            }
        }
    }
    async fn read_data(&self, query: &str) -> serde_json::Value {
        let mut response = self.execute(query).await.expect("snapshot query as actor");
        response["data"].take()
    }
}
