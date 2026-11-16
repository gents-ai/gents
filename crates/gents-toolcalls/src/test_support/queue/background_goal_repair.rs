use super::*;
pub async fn fixture(name: &str) -> (TestDb, CanonicalBackgroundFixture) {
    let db = test_db(name).await;
    let fixture = canonical_background_fixture(&db, name).await;
    (db, fixture)
}

pub async fn message_ids(db: &TestDb, key: &str) -> Vec<String> {
    let key = escape_graphql_string(key);
    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{message_key: {{_eq: "{key}"}}}}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    response["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["_docID"].as_str().unwrap().to_owned())
        .collect()
}

pub fn hints(parent: &AgentRequest) -> RequestQueue {
    RequestQueue {
        delivery: Default::default(),
        position: None,
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}
