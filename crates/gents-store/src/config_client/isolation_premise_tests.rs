//! External premise: what the pinned DefraDB validates when a transaction
//! commits after a concurrent one (Lean `Store.TransactionFence`). In every
//! race the deciding transaction T1 runs in a `transact_*` owner, and after its
//! first mutation a second writer T2 commits on its own before T1 commits;
//! `attempts == 2` means T1 met a confirmed conflict and its owner ran the
//! callback again. A two-raw-transaction unique-index commit is evidenced by
//! `txn_owner_tests::defradb_refuses_a_concurrent_duplicate_after_read_then_insert`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::{ConfigAccess, ConfigApplyTxn};
use crate::identity::NodeIdentity;

const SDL: &str = "type IsolationProbe { key: String grp: String n: Int m: Int }";
const SDL_UNIQUE: &str = "type UniqueIsolationProbe { key: String @index(unique: true) n: Int }";
const ARRIVAL_HEAD: &str =
    r#"{ _documentArrivals(collection: "IsolationProbe", limit: 1) { head } }"#;

async fn node() -> (Arc<EmbeddedNode>, ConfigAccess) {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    let access = ConfigAccess::local(Arc::clone(&node));
    access.add_collection(SDL).await.unwrap();
    access.add_collection(SDL_UNIQUE).await.unwrap();
    (node, access)
}

async fn client_signed_node() -> (Arc<EmbeddedNode>, ConfigAccess, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("probe.key");
    let identity = tokio::task::spawn_blocking(move || {
        crate::identity::KeyIdentity::load_or_create(key, None)
    })
    .await
    .unwrap()
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    let local = ConfigAccess::local(Arc::clone(&node));
    local.add_collection(SDL).await.unwrap();
    local.add_collection(SDL_UNIQUE).await.unwrap();
    let access = ConfigAccess::Graphql(
        super::GraphqlEndpoint::as_principal(endpoint, identity.did())
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap(),
    );
    (node, access, root)
}

async fn create(node: &Arc<EmbeddedNode>, collection: &str, input: &str) -> String {
    let created = ConfigAccess::write_local(
        node,
        "test.isolation_premise.seed",
        &format!("mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"),
    )
    .await
    .unwrap();
    crate::graphql::created_doc_id(&created, collection).unwrap()
}

async fn write(node: &Arc<EmbeddedNode>, mutation: String) {
    ConfigAccess::write_local(node, "test.isolation_premise.concurrent", &mutation)
        .await
        .unwrap();
}

fn rows(response: &Value, collection: &str) -> Vec<Value> {
    response["data"][collection]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn stored(node: &Arc<EmbeddedNode>, query: &str, collection: &str) -> Vec<Value> {
    let response = ConfigAccess::local(Arc::clone(node))
        .execute(query)
        .await
        .unwrap();
    rows(&response, collection)
}

#[derive(Debug)]
struct Observed {
    attempts: usize,
    result: Result<(), String>,
}

type Decide = dyn for<'t> Fn(&'t ConfigApplyTxn<'_>, usize) -> BoxFuture<'t, anyhow::Result<()>>
    + Send
    + Sync;

fn decider<F>(decide: F) -> Box<Decide>
where
    F: for<'t> Fn(&'t ConfigApplyTxn<'_>, usize) -> BoxFuture<'t, anyhow::Result<()>>
        + Send
        + Sync
        + 'static,
{
    Box::new(decide)
}

#[derive(Clone, Copy)]
enum Owner {
    Embedded,
    ClientSigned,
}

/// T1 runs `decide` in its transaction owner; `concurrent` (T2) commits after
/// T1's first mutation and before T1's commit.
async fn race<W>(access: &ConfigAccess, owner: Owner, decide: &Decide, concurrent: W) -> Observed
where
    W: std::future::Future<Output = ()>,
{
    let attempts = AtomicUsize::new(0);
    let (result, paused) = match owner {
        Owner::Embedded => {
            ConfigApplyTxn::with_concurrent_write_before_commit(
                access.transact_write("test.isolation_premise.decide", |txn| {
                    decide(txn, attempts.fetch_add(1, Ordering::SeqCst))
                }),
                concurrent,
            )
            .await
        }
        Owner::ClientSigned => {
            ConfigApplyTxn::with_concurrent_write_before_commit(
                access.transact_signed_conflict_rebuild(
                    "test.isolation_premise.decide_signed",
                    |txn| decide(txn, attempts.fetch_add(1, Ordering::SeqCst)),
                ),
                concurrent,
            )
            .await
        }
    };
    assert!(paused, "the concurrent writer must commit inside T1");
    Observed {
        attempts: attempts.load(Ordering::SeqCst),
        result: result.map_err(|error: anyhow::Error| format!("{error:#}")),
    }
}

fn insert_if_absent(collection: &'static str, key: &'static str) -> Box<Decide> {
    decider(move |txn, attempt| {
        Box::pin(async move {
            let found = txn
                .execute(&format!(
                    "{{ {collection}(filter: {{key: {{_eq: \"{key}\"}}}}) {{ _docID }} }}"
                ))
                .await?;
            if rows(&found, collection).is_empty() {
                txn.execute(&format!(
                    "mutation {{ create_{collection}(input: {{key: \"{key}\", n: {}}}) {{ _docID }} }}",
                    100 + attempt
                ))
                .await?;
            }
            Ok(())
        })
    })
}

async fn absent_key_race(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    owner: Owner,
    collection: &'static str,
    key: &'static str,
) -> (Observed, usize) {
    let observed = race(
        access,
        owner,
        &insert_if_absent(collection, key),
        write(
            node,
            format!(
                "mutation {{ create_{collection}(input: {{key: \"{key}\", n: 2}}) {{ _docID }} }}"
            ),
        ),
    )
    .await;
    let keys = stored(
        node,
        &format!("{{ {collection}(filter: {{key: {{_eq: \"{key}\"}}}}) {{ _docID }} }}"),
        collection,
    )
    .await
    .len();
    (observed, keys)
}

/// T1 reads document A by `selector`, writes document B; T2 updates A.
async fn read_a_write_b(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    owner: Owner,
    tag: &str,
    selector: &'static str,
) -> Observed {
    let key_a = format!("{tag}-a");
    let key_b = format!("{tag}-b");
    let a = create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{key_a}", n: 1, m: 1}}"#),
    )
    .await;
    let b = create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{key_b}", n: 1, m: 1}}"#),
    )
    .await;
    let read = match selector {
        "docID" => format!("{{ IsolationProbe(docID: \"{a}\") {{ n }} }}"),
        "docIDs" => format!("{{ IsolationProbe(docIDs: [\"{a}\"]) {{ n }} }}"),
        "_docID filter" => {
            format!("{{ IsolationProbe(filter: {{_docID: {{_eq: \"{a}\"}}}}, limit: 1) {{ n }} }}")
        }
        _ => format!(r#"{{ IsolationProbe(filter: {{key: {{_eq: "{key_a}"}}}}) {{ n }} }}"#),
    };
    let decide = decider(move |txn, _| {
        let read = read.clone();
        let b = b.clone();
        Box::pin(async move {
            let found = txn.execute(&read).await?;
            let n = rows(&found, "IsolationProbe")[0]["n"]
                .as_i64()
                .unwrap_or(-1);
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{b}\", input: {{n: {}}}) {{ _docID }} }}",
                n + 10
            ))
            .await?;
            Ok(())
        })
    });
    race(
        access,
        owner,
        &decide,
        write(
            node,
            format!("mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 5}}) {{ _docID }} }}"),
        ),
    )
    .await
}

/// T1 reads the members of `grp = grp` and writes an unrelated document C; T2
/// changes one member as `change` names.
async fn set_read_then_member_changes(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    owner: Owner,
    grp: &str,
    change: &'static str,
) -> Observed {
    let a = create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{grp}-a", grp: "{grp}", n: 1}}"#),
    )
    .await;
    create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{grp}-b", grp: "{grp}", n: 1}}"#),
    )
    .await;
    let c = create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{grp}-c", grp: "{grp}-summary", n: 0}}"#),
    )
    .await;
    let concurrent = match change {
        "leave" => format!(
            "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{grp: \"other\"}}) {{ _docID }} }}"
        ),
        "value" => format!(
            "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 9}}) {{ _docID }} }}"
        ),
        "delete" => format!("mutation {{ delete_IsolationProbe(docID: \"{a}\") {{ _docID }} }}"),
        _ => format!(
            r#"mutation {{ create_IsolationProbe(input: {{key: "{grp}-d", grp: "{grp}", n: 1}}) {{ _docID }} }}"#
        ),
    };
    let grp_owned = grp.to_owned();
    let decide = decider(move |txn, _| {
        let c = c.clone();
        let grp_owned = grp_owned.clone();
        Box::pin(async move {
            let found = txn
                .execute(&format!(
                    r#"{{ IsolationProbe(filter: {{grp: {{_eq: "{grp_owned}"}}}}) {{ _docID n }} }}"#
                ))
                .await?;
            let total: i64 = rows(&found, "IsolationProbe")
                .iter()
                .map(|row| row["n"].as_i64().unwrap_or(0))
                .sum();
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{c}\", input: {{n: {total}}}) {{ _docID }} }}"
            ))
            .await?;
            Ok(())
        })
    });
    race(access, owner, &decide, write(node, concurrent)).await
}

/// T1 reads document A by `selector` and writes `t1_writes` of A; T2 writes A.n.
async fn same_document_writers(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    key: &str,
    selector: &'static str,
    t1_writes: &'static str,
) -> (Observed, Value) {
    let a = create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{key}", n: 1, m: 1}}"#),
    )
    .await;
    let read = match selector {
        "docID" => format!("{{ IsolationProbe(docID: \"{a}\") {{ n m }} }}"),
        _ => format!(
            r#"{{ IsolationProbe(filter: {{key: {{_eq: "{key}"}}, n: {{_eq: 1}}}}) {{ _docID n m }} }}"#
        ),
    };
    let target = a.clone();
    let decide = decider(move |txn, _| {
        let read = read.clone();
        let a = target.clone();
        Box::pin(async move {
            if rows(&txn.execute(&read).await?, "IsolationProbe").is_empty() {
                return Ok(());
            }
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{{t1_writes}: 7}}) {{ _docID }} }}"
            ))
            .await?;
            Ok(())
        })
    });
    let observed = race(
        access,
        Owner::Embedded,
        &decide,
        write(
            node,
            format!("mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 2}}) {{ _docID }} }}"),
        ),
    )
    .await;
    let after = stored(
        node,
        &format!("{{ IsolationProbe(docID: \"{a}\") {{ n m }} }}"),
        "IsolationProbe",
    )
    .await;
    (observed, after[0].clone())
}

/// The id a create of `input` takes in `node`: DefraDB derives it from the
/// genesis block, so a discarded create names it without storing it.
async fn id_of_create(node: &Arc<EmbeddedNode>, input: &str) -> String {
    let txn = ConfigApplyTxn::begin_local(node, None).await.unwrap();
    let created = txn
        .execute(&format!(
            "mutation {{ create_IsolationProbe(input: {input}) {{ _docID }} }}"
        ))
        .await
        .unwrap();
    txn.discard().await.unwrap();
    crate::graphql::created_doc_id(&created, "IsolationProbe").unwrap()
}

/// T1 reads by id a document that does not exist yet and writes document B;
/// T2 creates the document with that id and commits first.
async fn absent_point_read_then_create(
    node: &Arc<EmbeddedNode>,
    access: &ConfigAccess,
    owner: Owner,
    key: &str,
) -> (Observed, String, String) {
    let input = format!(r#"{{key: "{key}", n: 1, m: 1}}"#);
    let absent = id_of_create(node, &input).await;
    assert!(stored(
        node,
        &format!("{{ IsolationProbe(docID: \"{absent}\") {{ n }} }}"),
        "IsolationProbe"
    )
    .await
    .is_empty());
    let b = create(
        node,
        "IsolationProbe",
        &format!(r#"{{key: "{key}-b", n: 1, m: 1}}"#),
    )
    .await;
    let read = format!("{{ IsolationProbe(docID: \"{absent}\") {{ n }} }}");
    let decide = decider(move |txn, _| {
        let read = read.clone();
        let b = b.clone();
        Box::pin(async move {
            let found = txn.execute(&read).await?;
            let n = rows(&found, "IsolationProbe").len();
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{b}\", input: {{n: {n}}}) {{ _docID }} }}"
            ))
            .await?;
            Ok(())
        })
    });
    let created = std::sync::Mutex::new(None);
    let observed = race(access, owner, &decide, async {
        *created.lock().unwrap() = Some(create(node, "IsolationProbe", &input).await);
    })
    .await;
    (observed, absent, created.into_inner().unwrap().unwrap())
}

#[tokio::test]
async fn embedded_isolation_premises() {
    let (node, access) = node().await;

    let (observed, keys) = absent_key_race(
        &node,
        &access,
        Owner::Embedded,
        "IsolationProbe",
        "absent-k",
    )
    .await;
    assert_eq!((observed.attempts, keys), (1, 2), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");

    let raw_read = r#"{ IsolationProbe(filter: {key: {_eq: "raw"}}) { _docID } }"#;
    let first = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    let second = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    for txn in [&first, &second] {
        assert!(rows(&txn.execute(raw_read).await.unwrap(), "IsolationProbe").is_empty());
    }
    for (txn, writer) in [(&first, 1), (&second, 2)] {
        txn.execute(&format!(
            "mutation {{ create_IsolationProbe(input: {{key: \"raw\", n: {writer}}}) {{ _docID }} }}"
        ))
        .await
        .unwrap();
    }
    first.commit().await.unwrap();
    second.commit().await.unwrap();
    assert_eq!(stored(&node, raw_read, "IsolationProbe").await.len(), 2);

    let (observed, keys) = absent_key_race(
        &node,
        &access,
        Owner::Embedded,
        "UniqueIsolationProbe",
        "uniq-read",
    )
    .await;
    assert_eq!((observed.attempts, keys), (2, 1), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");

    let blind = decider(|txn, attempt| {
        Box::pin(async move {
            txn.execute(&format!(
                "mutation {{ create_UniqueIsolationProbe(input: {{key: \"uniq-blind\", n: {}}}) {{ _docID }} }}",
                100 + attempt
            ))
            .await?;
            Ok(())
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &blind,
        write(
            &node,
            r#"mutation { create_UniqueIsolationProbe(input: {key: "uniq-blind", n: 2}) { _docID } }"#
                .into(),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert!(
        observed
            .result
            .as_ref()
            .is_err_and(|error| error.contains("unique index")),
        "{observed:?}"
    );
    assert_eq!(
        stored(
            &node,
            r#"{ UniqueIsolationProbe(filter: {key: {_eq: "uniq-blind"}}) { n } }"#,
            "UniqueIsolationProbe"
        )
        .await,
        vec![json!({"n": 2})]
    );

    for (tag, selector, expected_attempts) in [
        ("pt-docid", "docID", 2),
        ("pt-filter-id", "_docID filter", 2),
        ("pt-docids", "docIDs", 2),
        ("pt-filter-key", "filter", 1),
    ] {
        let observed = read_a_write_b(&node, &access, Owner::Embedded, tag, selector).await;
        assert_eq!(observed.attempts, expected_attempts, "{tag}: {observed:?}");
    }

    // Unique-key equality filter answers from the unique index and validates like a point read
    // (Lean `TransactionFence.a_unique_key_read_is_a_point_read`).
    let a = create(&node, "UniqueIsolationProbe", r#"{key: "uniq-pt-a", n: 1}"#).await;
    let b = create(&node, "IsolationProbe", r#"{key: "uniq-pt-b", n: 1, m: 1}"#).await;
    let decide = decider(move |txn, _| {
        let b = b.clone();
        Box::pin(async move {
            let found = txn
                .execute(
                    r#"{ UniqueIsolationProbe(filter: {key: {_eq: "uniq-pt-a"}}, limit: 2) { n } }"#,
                )
                .await?;
            let n = rows(&found, "UniqueIsolationProbe")[0]["n"]
                .as_i64()
                .unwrap_or(-1);
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{b}\", input: {{n: {}}}) {{ _docID }} }}",
                n + 10
            ))
            .await?;
            Ok(())
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &decide,
        write(
            &node,
            format!("mutation {{ update_UniqueIsolationProbe(docID: \"{a}\", input: {{n: 5}}) {{ _docID }} }}"),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 2, "{observed:?}");

    for (grp, change, expected_attempts) in [
        ("set-leave", "leave", 1),
        ("set-value", "value", 1),
        ("set-join", "join", 1),
        ("set-delete", "delete", 2),
    ] {
        let observed =
            set_read_then_member_changes(&node, &access, Owner::Embedded, grp, change).await;
        assert_eq!(observed.attempts, expected_attempts, "{grp}: {observed:?}");
    }

    let (observed, after) = same_document_writers(&node, &access, "same-docid", "docID", "m").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(after, json!({"n": 2, "m": 7}));

    let (observed, after) =
        same_document_writers(&node, &access, "same-filter-m", "filter", "m").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(after, json!({"n": 2, "m": 1}));

    let (observed, after) =
        same_document_writers(&node, &access, "same-filter-n", "filter", "n").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(after, json!({"n": 2, "m": 1}));

    let queued = create(
        &node,
        "IsolationProbe",
        r#"{key: "move-a", grp: "queued", n: 1, m: 1}"#,
    )
    .await;
    let claim = decider(|txn, _| {
        Box::pin(async move {
            txn.execute(
                r#"mutation { update_IsolationProbe(filter: {grp: {_eq: "queued"}}, input: {m: 7}) { _docID } }"#,
            )
            .await?;
            Ok(())
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &claim,
        write(
            &node,
            format!("mutation {{ update_IsolationProbe(docID: \"{queued}\", input: {{grp: \"cancelled\"}}) {{ _docID }} }}"),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(
        stored(
            &node,
            &format!("{{ IsolationProbe(docID: \"{queued}\") {{ grp m }} }}"),
            "IsolationProbe"
        )
        .await,
        vec![json!({"grp": "cancelled", "m": 1})]
    );

    create(
        &node,
        "IsolationProbe",
        r#"{key: "slot-x", grp: "slot", n: 1}"#,
    )
    .await;
    let admit = decider(|txn, attempt| {
        Box::pin(async move {
            let found = txn
                .execute(r#"{ running: COUNT(IsolationProbe: {filter: {grp: {_eq: "slot"}}}) }"#)
                .await?;
            if found["data"]["running"].as_i64().unwrap_or(i64::MAX) < 2 {
                txn.execute(&format!(
                    "mutation {{ create_IsolationProbe(input: {{key: \"t1-{attempt}\", grp: \"slot\", n: 1}}) {{ _docID }} }}"
                ))
                .await?;
            }
            Ok(())
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &admit,
        write(
            &node,
            r#"mutation { create_IsolationProbe(input: {key: "t2", grp: "slot", n: 1}) { _docID } }"#
                .into(),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
    assert_eq!(
        stored(
            &node,
            r#"{ IsolationProbe(filter: {grp: {_eq: "slot"}}) { _docID } }"#,
            "IsolationProbe"
        )
        .await
        .len(),
        3,
        "a limit of 2 admits 3 rows"
    );

    let arrival_insert = decider(|txn, attempt| {
        Box::pin(async move {
            txn.execute(ARRIVAL_HEAD).await?;
            insert_if_absent("IsolationProbe", "arrival-k")(txn, attempt).await
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &arrival_insert,
        write(
            &node,
            r#"mutation { create_IsolationProbe(input: {key: "arrival-k", n: 2}) { _docID } }"#
                .into(),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
    assert_eq!(
        stored(
            &node,
            r#"{ IsolationProbe(filter: {key: {_eq: "arrival-k"}}) { _docID } }"#,
            "IsolationProbe"
        )
        .await
        .len(),
        2
    );

    let arr_a = create(
        &node,
        "IsolationProbe",
        r#"{key: "arr-u-a", grp: "arr-g", n: 1}"#,
    )
    .await;
    let arr_c = create(
        &node,
        "IsolationProbe",
        r#"{key: "arr-u-c", grp: "arr-summary", n: 0}"#,
    )
    .await;
    let arrival_update = decider(move |txn, _| {
        let arr_c = arr_c.clone();
        Box::pin(async move {
            txn.execute(ARRIVAL_HEAD).await?;
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{arr_c}\", input: {{n: 1}}) {{ _docID }} }}"
            ))
            .await?;
            Ok(())
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &arrival_update,
        write(
            &node,
            format!("mutation {{ update_IsolationProbe(docID: \"{arr_a}\", input: {{grp: \"other\"}}) {{ _docID }} }}"),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 1, "{observed:?}");

    let committed_inside = Arc::new(AtomicBool::new(false));
    let observed_inside = Arc::clone(&committed_inside);
    let gate_first = decider(|txn, _| {
        Box::pin(async move {
            txn.execute(
                r#"mutation { create_IsolationProbe(input: {key: "gate-first", grp: "gate"}) { _docID } }"#,
            )
            .await?;
            Ok(())
        })
    });
    let gate_second = async {
        write(
            &node,
            r#"mutation { create_IsolationProbe(input: {key: "gate-second", grp: "gate"}) { _docID } }"#
                .into(),
        )
        .await;
        observed_inside.store(true, Ordering::SeqCst);
    };
    let observed = race(&access, Owner::Embedded, &gate_first, gate_second).await;
    assert!(observed.result.is_ok(), "{observed:?}");
    assert!(committed_inside.load(Ordering::SeqCst));
    assert_eq!(
        stored(
            &node,
            r#"{ IsolationProbe(filter: {grp: {_eq: "gate"}}) { key } }"#,
            "IsolationProbe"
        )
        .await
        .len(),
        2
    );

    let (observed, absent, created_id) =
        absent_point_read_then_create(&node, &access, Owner::Embedded, "absent-pt").await;
    assert_eq!(created_id, absent, "the create takes the id T1 read");
    assert_eq!(observed.attempts, 2, "{observed:?}");

    let ro_doc = create(&node, "IsolationProbe", r#"{key: "ro-a", n: 1, m: 1}"#).await;
    let reader = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    let ro_read = format!("{{ IsolationProbe(docID: \"{ro_doc}\") {{ n }} }}");
    assert_eq!(
        rows(&reader.execute(&ro_read).await.unwrap(), "IsolationProbe")[0]["n"],
        1
    );
    write(
        &node,
        format!("mutation {{ update_IsolationProbe(docID: \"{ro_doc}\", input: {{n: 5}}) {{ _docID }} }}"),
    )
    .await;
    reader.commit().await.unwrap();

    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn client_signed_isolation_premises() {
    let (node, access, _root) = client_signed_node().await;

    let (observed, keys) = absent_key_race(
        &node,
        &access,
        Owner::ClientSigned,
        "IsolationProbe",
        "cs-absent",
    )
    .await;
    assert_eq!((observed.attempts, keys), (1, 2), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");

    let (observed, keys) = absent_key_race(
        &node,
        &access,
        Owner::ClientSigned,
        "UniqueIsolationProbe",
        "cs-uniq",
    )
    .await;
    assert_eq!((observed.attempts, keys), (2, 1), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");

    let observed = read_a_write_b(&node, &access, Owner::ClientSigned, "cs-pt", "docID").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");

    let observed =
        set_read_then_member_changes(&node, &access, Owner::ClientSigned, "cs-set-leave", "leave")
            .await;
    assert_eq!(observed.attempts, 1, "{observed:?}");

    let (observed, absent, created_id) =
        absent_point_read_then_create(&node, &access, Owner::ClientSigned, "cs-absent-pt").await;
    assert_eq!(created_id, absent, "the create takes the id T1 read");
    assert_eq!(observed.attempts, 2, "{observed:?}");

    node.shutdown().await.unwrap();
}

/// A document's id is the CID of its genesis block, which links its signer's
/// signature: the same create signed by another key takes another id, and the
/// same key signing the same create takes the same id. So no actor but the
/// signer can name a document's id before the creating transaction commits
/// (the premise `Identity.SessionInterrupt.marker_squatting_needs_the_creators_key`
/// rests on).
#[tokio::test]
async fn a_signed_create_takes_an_id_only_its_signer_can_derive() {
    let directory = tempfile::tempdir().unwrap();
    let root = Arc::new(
        crate::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap(),
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    ConfigAccess::local(Arc::clone(&node))
        .add_collection(SDL)
        .await
        .unwrap();
    let runtime = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap());
    let mut signers = Vec::new();
    for _ in 0..2 {
        let binding = crate::identity::provision_agent_identity(root.as_ref()).unwrap();
        let agent = crate::identity::resolve_agent_identity(root.as_ref(), &binding).unwrap();
        crate::identity::GrantSet::AgentBaseline
            .grant(&runtime, &identity::Did::new(agent.did()).unwrap())
            .await
            .unwrap();
        signers.push(
            ConfigAccess::local_managed_agent(
                node.clone(),
                identity::Did::new(agent.did()).unwrap(),
                agent.as_ref(),
            )
            .unwrap(),
        );
    }
    async fn signed_create(access: &ConfigAccess) -> anyhow::Result<String> {
        access
            .transact_write("test.isolation_premise.signed_create", |txn| {
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: IsolationProbeMutationInputArg!) { create_IsolationProbe(input: $input) { _docID } }",
                            &json!({"input": {"key": "session", "n": 1, "m": 1}}),
                            SDL,
                        )
                        .await?;
                    Ok(crate::graphql::created_doc_id(&created, "IsolationProbe")?)
                })
            })
            .await
    }
    let first = signed_create(&signers[0]).await.unwrap();
    let second = signed_create(&signers[1]).await.unwrap();
    assert_ne!(
        first, second,
        "another signer's identical create takes another id"
    );
    let again = signed_create(&signers[0]).await;
    assert!(
        again.is_err(),
        "the same signer's identical create names the id it already holds: {again:?}"
    );
    node.shutdown().await.unwrap();
}
