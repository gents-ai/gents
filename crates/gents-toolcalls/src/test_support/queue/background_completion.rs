use super::*;
pub async fn agent_data(db: &TestDb, query: &str) -> serde_json::Value {
    db.agent_access.execute(query).await.unwrap()["data"].clone()
}

pub async fn agent_write(db: &TestDb, operation: &'static str, mutation: &str) {
    db.agent_access
        .transact_write(operation, |txn| {
            Box::pin(async move { Ok::<_, anyhow::Error>(txn.execute(mutation).await?) })
        })
        .await
        .unwrap();
}
