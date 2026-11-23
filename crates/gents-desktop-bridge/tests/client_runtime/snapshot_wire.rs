//! How many HTTP requests a Node's first full read makes. Every request a
//! phone sends crosses a relay, so the count is part of the read's contract.

use gents::client_test_support::ServedRuntimeHome;
use gents_observe::client_projection::query::load_node_snapshot_on;
use gents_observe::client_projection::store::ClientStore;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A loopback HTTP relay that forwards every byte to the runtime and keeps
/// each GraphQL document it carried. The Host header passes through, so the
/// bearer's audience is the relay's own address.
struct WireRecorder {
    url: String,
    documents: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl WireRecorder {
    async fn start(upstream: &str) -> Self {
        let parsed = reqwest::Url::parse(upstream).unwrap();
        let target = format!(
            "{}:{}",
            parsed.host_str().unwrap(),
            parsed.port_or_known_default().unwrap()
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}{}", listener.local_addr().unwrap(), parsed.path());
        let documents = Arc::new(Mutex::new(Vec::new()));
        let log = documents.clone();
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(server) = tokio::net::TcpStream::connect(&target).await else {
                    continue;
                };
                tokio::spawn(relay(client, server, log.clone()));
            }
        });
        Self {
            url,
            documents,
            task,
        }
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.documents.lock().unwrap())
    }
}

impl Drop for WireRecorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay(
    client: tokio::net::TcpStream,
    server: tokio::net::TcpStream,
    log: Arc<Mutex<Vec<String>>>,
) {
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let answers = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut server_read, &mut client_write).await;
        let _ = client_write.shutdown().await;
    });
    let mut pending = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let read = match client_read.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if server_write.write_all(&chunk[..read]).await.is_err() {
            break;
        }
        pending.extend_from_slice(&chunk[..read]);
        while let Some(body) = complete_request(&mut pending) {
            let document = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|body| body["query"].as_str().map(str::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
            log.lock().unwrap().push(document);
        }
    }
    let _ = server_write.shutdown().await;
    let _ = answers.await;
}

/// Removes one whole request from the front of `pending` and returns its body.
fn complete_request(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let head_end = pending
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&pending[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(|value| value.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    let end = head_end + 4 + length;
    if pending.len() < end {
        return None;
    }
    let body = pending[head_end + 4..end].to_vec();
    pending.drain(..end);
    Some(body)
}

/// The collections a document reads, one entry per root field.
fn root_collections(document: &str) -> Vec<String> {
    let body = document
        .find('{')
        .map(|start| &document[start..])
        .unwrap_or(document);
    let mut roots = Vec::new();
    let (mut braces, mut parens) = (0usize, 0usize);
    let mut word = String::new();
    for character in body.chars() {
        match character {
            '{' => {
                if braces == 1 && parens == 0 && !word.is_empty() {
                    roots.push(std::mem::take(&mut word));
                }
                braces += 1;
            }
            '}' => braces -= 1,
            '(' => {
                if braces == 1 && parens == 0 && !word.is_empty() {
                    roots.push(std::mem::take(&mut word));
                }
                parens += 1;
            }
            ')' => parens -= 1,
            character if braces == 1 && parens == 0 => {
                if character.is_alphanumeric() || character == '_' {
                    word.push(character);
                } else if !character.is_whitespace() {
                    /* an alias's colon or any other separator ends the word */
                    word.clear();
                }
            }
            _ => {}
        }
    }
    roots
}

fn describe(document: &str) -> String {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for root in root_collections(document) {
        *counts.entry(root).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(collection, count)| match count {
            1 => collection,
            count => format!("{collection} x{count}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn create(runtime: &ServedRuntimeHome, collection: &str, value: serde_json::Value) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
    let response = runtime
        .user_graphql_access()
        .write(
            "test.snapshot_wire.seed",
            &format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    gents_store::graphql::created_doc_id(&response, collection).unwrap()
}

/// Creates `values` in one mutation and returns their document ids in order.
async fn create_many(
    runtime: &ServedRuntimeHome,
    collection: &str,
    values: &[serde_json::Value],
) -> Vec<String> {
    let creates = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let input = gents_protocol::graphql::graphql_input_literal(value).unwrap();
            format!("c{index}: create_{collection}(input: {input}) {{ _docID }}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = runtime
        .user_graphql_access()
        .write(
            "test.snapshot_wire.seed",
            &format!("mutation {{ {creates} }}"),
        )
        .await
        .unwrap();
    let data = response.get("data").unwrap_or(&response);
    (0..values.len())
        .map(|index| {
            let created = &data[format!("c{index}")];
            let created = created.get(0).unwrap_or(created);
            created["_docID"].as_str().unwrap().to_owned()
        })
        .collect()
}

/// Sessions beyond the edge cases, each with one completed head, as a Node
/// someone has used for a while has.
const PLAIN_SESSIONS: usize = 150;

/// A Node with a few sessions whose heads reach every closure edge
/// (retry parent, queued predecessor, replacement, retry children) and a
/// trigger whose candidate page is full, so its cutoff is refined.
async fn seed(runtime: &ServedRuntimeHome) {
    let node = runtime.node_did().to_owned();
    let requester = runtime.user().did().to_owned();
    let request = |session: &str, id: &str, state: &str, at: u32| {
        json!({
            "request_id": id, "purpose": "normal", "node_did": node,
            "requester_did": requester, "agent_did": "default", "session_id": session,
            "content": format!("{id} content"), "lifecycle_state": state,
            "created_at": format!("2026-11-01T00:00:{at:02}Z"),
        })
    };
    let mut heads = Vec::new();
    let plain = (0..4).chain(8..8 + PLAIN_SESSIONS).collect::<Vec<_>>();
    let docs = create_many(
        runtime,
        "AgentRequest",
        &plain
            .iter()
            .map(|index| {
                request(
                    &format!("session-{index}"),
                    &format!("session-{index}-a"),
                    "completed",
                    1,
                )
            })
            .collect::<Vec<_>>(),
    )
    .await;
    for (index, doc) in plain.iter().zip(docs) {
        heads.push((
            format!("session-{index}"),
            doc,
            format!("session-{index}-a"),
            "completed",
        ));
    }

    let failed = create(
        runtime,
        "AgentRequest",
        request("session-4", "session-4-a", "failed", 1),
    )
    .await;
    let mut retry = request("session-4", "session-4-b", "completed", 2);
    retry["retry_parent_request"] = "session-4-a".into();
    retry["retry_parent_request_doc_id"] = failed.into();
    let retry = create(runtime, "AgentRequest", retry).await;
    heads.push(("session-4".into(), retry, "session-4-b".into(), "completed"));

    create(
        runtime,
        "AgentRequest",
        request("session-5", "session-5-a", "processing", 1),
    )
    .await;
    let mut queued = request("session-5", "session-5-b", "pending", 2);
    queued["input"] = serde_json::to_value(gents_protocol::request_input::RequestInput {
        queue: Some(gents_protocol::request_input::RequestQueue {
            delivery: Default::default(),
            position: None,
            source: gents_protocol::request_input::QueueSource::User,
            policy: gents_protocol::request_input::QueuePolicy::Append,
            key: None,
            queued_after_request_id: Some("session-5-a".into()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        }),
        ..Default::default()
    })
    .unwrap();
    let queued = create(runtime, "AgentRequest", queued).await;
    heads.push(("session-5".into(), queued, "session-5-b".into(), "pending"));

    let replacement = create(
        runtime,
        "AgentRequest",
        request("session-6", "session-6-b", "completed", 2),
    )
    .await;
    let mut replaced = request("session-6", "session-6-a", "superseded", 1);
    replaced["failure_reason"] = "pending message replaced".into();
    replaced["superseded_by_request"] = "session-6-b".into();
    replaced["superseded_by_request_doc_id"] = replacement.into();
    let replaced = create(runtime, "AgentRequest", replaced).await;
    heads.push((
        "session-6".into(),
        replaced,
        "session-6-a".into(),
        "superseded",
    ));

    let sessions = (0..8 + PLAIN_SESSIONS)
        .map(|index| {
            let session = format!("session-{index}");
            let head = heads.iter().find(|(id, ..)| *id == session);
            let mut value = json!({
                "session_id": session, "node_did": node, "requester_did": requester,
                "agent_did": "default", "title": {"text": session, "source": "user"},
                "created_at": format!("2026-11-01T01:{:02}:{:02}Z", index / 60, index % 60),
            });
            if let Some((_, doc, id, state)) = head {
                value["observation"] = json!({
                    "last_activity_at": "2026-11-01T00:01:00Z",
                    "latest_request": {"request_doc_id": doc, "request_id": id, "lifecycle_state": state},
                });
            }
            value
        })
        .collect::<Vec<_>>();
    create_many(runtime, "AgentSession", &sessions).await;

    create(
        runtime,
        "Trigger",
        json!({
            "node_did": node, "trigger_id": "trigger-1", "task_id": "task-1",
            "source": {"kind": "schedule", "schedule_id": "schedule-1"}, "enabled": true,
        }),
    )
    .await;
    for index in 0..9 {
        let mut fire = request("task-session", &format!("fire-{index}"), "completed", index);
        fire["caused_by_trigger_id"] = "trigger-1".into();
        create(runtime, "AgentRequest", fire).await;
    }
}

/// The snapshot as rows, with this run's identities and document ids masked,
/// so two runs of the same seed compare equal.
fn normalized(store: &ClientStore, runtime: &ServedRuntimeHome) -> serde_json::Value {
    let text = serde_json::to_string(&store.to_rows())
        .unwrap()
        .replace(runtime.node_did(), "did:runtime")
        .replace(runtime.user().did(), "did:user");
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    fn mask(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) if text.starts_with("bae-") => *text = "bae-*".into(),
            serde_json::Value::Array(items) => items.iter_mut().for_each(mask),
            serde_json::Value::Object(fields) => fields.values_mut().for_each(mask),
            _ => {}
        }
    }
    mask(&mut value);
    if let serde_json::Value::Object(fields) = &mut value {
        for rows in fields.values_mut() {
            if let serde_json::Value::Array(rows) = rows {
                rows.sort_by_key(|row| row.to_string());
            }
        }
    }
    value
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_snapshot_read_request_count() {
    let runtime = ServedRuntimeHome::start().await;
    seed(&runtime).await;
    let recorder = WireRecorder::start(runtime.graphql()).await;
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(recorder.url.clone(), runtime.user().did())
            .with_client_signing_capability(runtime.user().client_signing_capability().unwrap())
            .unwrap(),
    );
    recorder.take();
    let store = load_node_snapshot_on(&access, runtime.node_did())
        .await
        .expect("snapshot");
    let documents = recorder.take();
    for (index, document) in documents.iter().enumerate() {
        eprintln!(
            "request {}: {} ({} bytes)",
            index + 1,
            describe(document),
            document.len()
        );
    }
    eprintln!("snapshot read made {} requests", documents.len());
    eprintln!(
        "sessions {} requests {} triggers {}",
        store.sessions.len(),
        store.requests.len(),
        store.triggers.len()
    );

    assert_eq!(store.sessions.len(), 8 + PLAIN_SESSIONS);
    assert_eq!(store.requests.len(), 168);
    assert_eq!(documents.len(), 5, "the first read's request count changed");
    // SNAPSHOT_WIRE_DUMP saves this run's snapshot; SNAPSHOT_WIRE_COMPARE
    // holds a later run (another revision) to the saved one.
    let snapshot = normalized(&store, &runtime);
    if let Ok(path) = std::env::var("SNAPSHOT_WIRE_DUMP") {
        std::fs::write(&path, serde_json::to_string_pretty(&snapshot).unwrap()).unwrap();
    }
    if let Ok(path) = std::env::var("SNAPSHOT_WIRE_COMPARE") {
        let before: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(snapshot, before, "the snapshot changed");
    }
    drop(recorder);
    runtime.shutdown().await;
}
