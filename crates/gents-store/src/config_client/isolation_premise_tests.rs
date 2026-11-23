//! External premise: what the pinned DefraDB validates when a transaction
//! commits after a concurrent one (Lean `Store.TransactionFence`). In every
//! race the deciding transaction T1 runs in a `transact_*` owner, and after its
//! first mutation a second writer T2 commits on its own before T1 commits;
//! `attempts == 2` means T1 met a confirmed conflict and its owner ran the
//! callback again. A two-raw-transaction unique-index commit is evidenced by
//! `txn_owner_tests::defradb_refuses_a_concurrent_duplicate_after_read_then_insert`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use super::{ConfigAccess, ConfigApplyTxn};
use crate::identity::NodeIdentity;

const SDL: &str = "type IsolationProbe { key: String grp: String n: Int m: Int }";
const SDL_UNIQUE: &str = "type UniqueIsolationProbe { key: String @index(unique: true) n: Int }";

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
    let identity =
        tokio::task::spawn_blocking(move || crate::identity::KeyIdentity::load_or_create(key, None))
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

fn insert_if_absent(collection: &'static str) -> Box<Decide> {
    decider(move |txn, attempt| {
        Box::pin(async move {
            let found = txn
                .execute(&format!(
                    "{{ {collection}(filter: {{key: {{_eq: \"k\"}}}}) {{ _docID }} }}"
                ))
                .await?;
            if rows(&found, collection).is_empty() {
                txn.execute(&format!(
                    "mutation {{ create_{collection}(input: {{key: \"k\", n: {}}}) {{ _docID }} }}",
                    100 + attempt
                ))
                .await?;
            }
            Ok(())
        })
    })
}

async fn absent_key_race(owner: Owner, collection: &'static str) -> (Observed, usize) {
    let (node, access, _root) = match owner {
        Owner::Embedded => {
            let (node, access) = node().await;
            (node, access, None)
        }
        Owner::ClientSigned => {
            let (node, access, root) = client_signed_node().await;
            (node, access, Some(root))
        }
    };
    let observed = race(
        &access,
        owner,
        &insert_if_absent(collection),
        write(
            &node,
            format!("mutation {{ create_{collection}(input: {{key: \"k\", n: 2}}) {{ _docID }} }}"),
        ),
    )
    .await;
    let keys = stored(
        &node,
        &format!("{{ {collection}(filter: {{key: {{_eq: \"k\"}}}}) {{ _docID }} }}"),
        collection,
    )
    .await
    .len();
    node.shutdown().await.unwrap();
    (observed, keys)
}

#[tokio::test]
async fn a_filtered_read_that_found_nothing_does_not_conflict_with_a_concurrent_insert() {
    let (observed, keys) = absent_key_race(Owner::Embedded, "IsolationProbe").await;
    assert_eq!((observed.attempts, keys), (1, 2), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");
}

#[tokio::test]
async fn a_client_signed_filtered_read_that_found_nothing_does_not_conflict_with_a_concurrent_insert(
) {
    let (observed, keys) = absent_key_race(Owner::ClientSigned, "IsolationProbe").await;
    assert_eq!((observed.attempts, keys), (1, 2), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");
}

#[tokio::test]
async fn two_raw_transactions_that_read_a_key_absent_both_commit_its_insert() {
    let (node, _access) = node().await;
    let read = r#"{ IsolationProbe(filter: {key: {_eq: "raw"}}) { _docID } }"#;
    let first = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    let second = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    for txn in [&first, &second] {
        assert!(rows(&txn.execute(read).await.unwrap(), "IsolationProbe").is_empty());
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
    assert_eq!(stored(&node, read, "IsolationProbe").await.len(), 2);
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_unique_index_conflicts_a_read_then_insert_and_the_rebuild_sees_the_winner() {
    let (observed, keys) = absent_key_race(Owner::Embedded, "UniqueIsolationProbe").await;
    assert_eq!((observed.attempts, keys), (2, 1), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");
}

#[tokio::test]
async fn a_client_signed_unique_index_conflict_is_rebuilt_and_the_rebuild_sees_the_winner() {
    let (observed, keys) = absent_key_race(Owner::ClientSigned, "UniqueIsolationProbe").await;
    assert_eq!((observed.attempts, keys), (2, 1), "{observed:?}");
    assert!(observed.result.is_ok(), "{observed:?}");
}

#[tokio::test]
async fn a_unique_index_conflicts_a_blind_insert_and_the_rebuild_fails_on_the_index() {
    let (node, access) = node().await;
    let blind = decider(|txn, attempt| {
        Box::pin(async move {
            txn.execute(&format!(
                "mutation {{ create_UniqueIsolationProbe(input: {{key: \"k\", n: {}}}) {{ _docID }} }}",
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
            r#"mutation { create_UniqueIsolationProbe(input: {key: "k", n: 2}) { _docID } }"#.into(),
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
        stored(&node, "{ UniqueIsolationProbe { n } }", "UniqueIsolationProbe").await,
        vec![json!({"n": 2})]
    );
    node.shutdown().await.unwrap();
}

/// T1 reads document A by `selector`, writes document B; T2 updates A.
async fn read_a_write_b(owner: Owner, selector: &'static str) -> Observed {
    let (node, access, _root) = match owner {
        Owner::Embedded => {
            let (node, access) = node().await;
            (node, access, None)
        }
        Owner::ClientSigned => {
            let (node, access, root) = client_signed_node().await;
            (node, access, Some(root))
        }
    };
    let a = create(&node, "IsolationProbe", r#"{key: "a", n: 1, m: 1}"#).await;
    let b = create(&node, "IsolationProbe", r#"{key: "b", n: 1, m: 1}"#).await;
    let read = match selector {
        "docID" => format!("{{ IsolationProbe(docID: \"{a}\") {{ n }} }}"),
        "docIDs" => format!("{{ IsolationProbe(docIDs: [\"{a}\"]) {{ n }} }}"),
        "_docID filter" => {
            format!("{{ IsolationProbe(filter: {{_docID: {{_eq: \"{a}\"}}}}, limit: 1) {{ n }} }}")
        }
        _ => r#"{ IsolationProbe(filter: {key: {_eq: "a"}}) { n } }"#.to_owned(),
    };
    let decide = decider(move |txn, _| {
        let read = read.clone();
        let b = b.clone();
        Box::pin(async move {
            let found = txn.execute(&read).await?;
            let n = rows(&found, "IsolationProbe")[0]["n"].as_i64().unwrap_or(-1);
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
        owner,
        &decide,
        write(
            &node,
            format!("mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 5}}) {{ _docID }} }}"),
        ),
    )
    .await;
    node.shutdown().await.unwrap();
    observed
}

#[tokio::test]
async fn a_point_read_by_doc_id_conflicts_with_a_concurrent_update_of_that_document() {
    let observed = read_a_write_b(Owner::Embedded, "docID").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

#[tokio::test]
async fn a_doc_id_equality_filter_is_a_point_read_that_conflicts_with_a_concurrent_update() {
    let observed = read_a_write_b(Owner::Embedded, "_docID filter").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

#[tokio::test]
async fn a_doc_ids_list_read_conflicts_with_a_concurrent_update_of_a_listed_document() {
    let observed = read_a_write_b(Owner::Embedded, "docIDs").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

#[tokio::test]
async fn a_client_signed_point_read_conflicts_with_a_concurrent_update_of_that_document() {
    let observed = read_a_write_b(Owner::ClientSigned, "docID").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

#[tokio::test]
async fn a_filtered_read_of_a_document_does_not_conflict_with_a_concurrent_update_of_it() {
    let observed = read_a_write_b(Owner::Embedded, "filter").await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
}

/// T1 reads the members of `grp = g` and writes an unrelated document C; T2
/// changes one member as `change` names.
async fn set_read_then_member_changes(owner: Owner, change: &'static str) -> Observed {
    let (node, access, _root) = match owner {
        Owner::Embedded => {
            let (node, access) = node().await;
            (node, access, None)
        }
        Owner::ClientSigned => {
            let (node, access, root) = client_signed_node().await;
            (node, access, Some(root))
        }
    };
    let a = create(&node, "IsolationProbe", r#"{key: "a", grp: "g", n: 1}"#).await;
    create(&node, "IsolationProbe", r#"{key: "b", grp: "g", n: 1}"#).await;
    let c = create(&node, "IsolationProbe", r#"{key: "c", grp: "summary", n: 0}"#).await;
    let concurrent = match change {
        "leave" => format!(
            "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{grp: \"other\"}}) {{ _docID }} }}"
        ),
        "value" => format!(
            "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 9}}) {{ _docID }} }}"
        ),
        "delete" => format!("mutation {{ delete_IsolationProbe(docID: \"{a}\") {{ _docID }} }}"),
        _ => r#"mutation { create_IsolationProbe(input: {key: "d", grp: "g", n: 1}) { _docID } }"#
            .to_owned(),
    };
    let decide = decider(move |txn, _| {
        let c = c.clone();
        Box::pin(async move {
            let found = txn
                .execute(r#"{ IsolationProbe(filter: {grp: {_eq: "g"}}) { _docID n } }"#)
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
    let observed = race(&access, owner, &decide, write(&node, concurrent)).await;
    node.shutdown().await.unwrap();
    observed
}

#[tokio::test]
async fn a_filtered_set_read_does_not_conflict_when_a_member_leaves_the_set() {
    let observed = set_read_then_member_changes(Owner::Embedded, "leave").await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
}

#[tokio::test]
async fn a_client_signed_filtered_set_read_does_not_conflict_when_a_member_leaves_the_set() {
    let observed = set_read_then_member_changes(Owner::ClientSigned, "leave").await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
}

#[tokio::test]
async fn a_filtered_set_read_does_not_conflict_when_a_member_value_changes() {
    let observed = set_read_then_member_changes(Owner::Embedded, "value").await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
}

#[tokio::test]
async fn a_filtered_set_read_does_not_conflict_when_a_member_joins_the_set() {
    let observed = set_read_then_member_changes(Owner::Embedded, "join").await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
}

#[tokio::test]
async fn a_filtered_set_read_conflicts_when_a_member_is_deleted() {
    let observed = set_read_then_member_changes(Owner::Embedded, "delete").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

/// T1 reads document A by `selector` and writes `t1_writes` of A; T2 writes A.n.
async fn same_document_writers(selector: &'static str, t1_writes: &'static str) -> (Observed, Value) {
    let (node, access) = node().await;
    let a = create(&node, "IsolationProbe", r#"{key: "a", n: 1, m: 1}"#).await;
    let read = match selector {
        "docID" => format!("{{ IsolationProbe(docID: \"{a}\") {{ n m }} }}"),
        _ => r#"{ IsolationProbe(filter: {n: {_eq: 1}}) { _docID n m } }"#.to_owned(),
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
        &access,
        Owner::Embedded,
        &decide,
        write(
            &node,
            format!("mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 2}}) {{ _docID }} }}"),
        ),
    )
    .await;
    let after = stored(
        &node,
        &format!("{{ IsolationProbe(docID: \"{a}\") {{ n m }} }}"),
        "IsolationProbe",
    )
    .await;
    node.shutdown().await.unwrap();
    (observed, after[0].clone())
}

#[tokio::test]
async fn writers_of_one_document_conflict_whatever_fields_they_write() {
    let (observed, after) = same_document_writers("docID", "m").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(after, json!({"n": 2, "m": 7}));
}

#[tokio::test]
async fn a_filtered_read_then_write_of_a_document_conflicts_with_a_concurrent_write_of_it() {
    let (observed, after) = same_document_writers("filter", "m").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(after, json!({"n": 2, "m": 1}));
}

#[tokio::test]
async fn a_filtered_read_then_write_of_one_field_conflicts_with_a_concurrent_write_of_that_field() {
    let (observed, after) = same_document_writers("filter", "n").await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(after, json!({"n": 2, "m": 1}));
}

#[tokio::test]
async fn a_filtered_update_conflicts_with_a_concurrent_move_out_of_its_filter() {
    let (node, access) = node().await;
    let a = create(&node, "IsolationProbe", r#"{key: "a", grp: "queued", n: 1, m: 1}"#).await;
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
            format!(
                "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{grp: \"cancelled\"}}) {{ _docID }} }}"
            ),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 2, "{observed:?}");
    assert_eq!(
        stored(
            &node,
            &format!("{{ IsolationProbe(docID: \"{a}\") {{ grp m }} }}"),
            "IsolationProbe"
        )
        .await,
        vec![json!({"grp": "cancelled", "m": 1})]
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_count_below_a_limit_does_not_conflict_with_a_concurrent_insert() {
    let (node, access) = node().await;
    create(&node, "IsolationProbe", r#"{key: "x", grp: "slot", n: 1}"#).await;
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
    let admitted = stored(
        &node,
        r#"{ IsolationProbe(filter: {grp: {_eq: "slot"}}) { _docID } }"#,
        "IsolationProbe",
    )
    .await;
    assert_eq!(admitted.len(), 3, "a limit of 2 admits 3 rows");
    node.shutdown().await.unwrap();
}

const ARRIVAL_HEAD: &str = r#"{ _documentArrivals(collection: "IsolationProbe", limit: 1) { head } }"#;

#[tokio::test]
async fn an_arrival_head_read_does_not_conflict_with_a_concurrent_insert() {
    let (node, access) = node().await;
    create(&node, "IsolationProbe", r#"{key: "seed", n: 0}"#).await;
    let decide = decider(|txn, attempt| {
        Box::pin(async move {
            txn.execute(ARRIVAL_HEAD).await?;
            insert_if_absent("IsolationProbe")(txn, attempt).await
        })
    });
    let observed = race(
        &access,
        Owner::Embedded,
        &decide,
        write(
            &node,
            r#"mutation { create_IsolationProbe(input: {key: "k", n: 2}) { _docID } }"#.into(),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
    assert_eq!(
        stored(
            &node,
            r#"{ IsolationProbe(filter: {key: {_eq: "k"}}) { _docID } }"#,
            "IsolationProbe"
        )
        .await
        .len(),
        2
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_arrival_head_read_does_not_conflict_with_a_concurrent_update() {
    let (node, access) = node().await;
    let a = create(&node, "IsolationProbe", r#"{key: "a", grp: "g", n: 1}"#).await;
    let c = create(&node, "IsolationProbe", r#"{key: "c", grp: "summary", n: 0}"#).await;
    let decide = decider(move |txn, _| {
        let c = c.clone();
        Box::pin(async move {
            txn.execute(ARRIVAL_HEAD).await?;
            txn.execute(&format!(
                "mutation {{ update_IsolationProbe(docID: \"{c}\", input: {{n: 1}}) {{ _docID }} }}"
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
            format!(
                "mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{grp: \"other\"}}) {{ _docID }} }}"
            ),
        ),
    )
    .await;
    assert_eq!(observed.attempts, 1, "{observed:?}");
    node.shutdown().await.unwrap();
}

/// The embedded write gate admits a second writer while the first holds an
/// open transaction: it orders nothing between local writers.
#[tokio::test]
async fn the_write_gate_lets_a_second_writer_commit_while_the_first_is_open() {
    let (node, access) = node().await;
    let committed_inside = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_inside = Arc::clone(&committed_inside);
    let decide = decider(|txn, _| {
        Box::pin(async move {
            txn.execute(r#"mutation { create_IsolationProbe(input: {key: "first"}) { _docID } }"#)
                .await?;
            Ok(())
        })
    });
    let second = async {
        write(
            &node,
            r#"mutation { create_IsolationProbe(input: {key: "second"}) { _docID } }"#.into(),
        )
        .await;
        observed_inside.store(true, Ordering::SeqCst);
    };
    let observed = race(&access, Owner::Embedded, &decide, second).await;
    assert!(observed.result.is_ok(), "{observed:?}");
    assert!(committed_inside.load(Ordering::SeqCst));
    assert_eq!(
        stored(&node, "{ IsolationProbe { key } }", "IsolationProbe")
            .await
            .len(),
        2
    );
    node.shutdown().await.unwrap();
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
async fn absent_point_read_then_create(owner: Owner) -> (Observed, String, String) {
    const INPUT: &str = r#"{key: "absent", n: 1, m: 1}"#;
    let (node, access, _root) = match owner {
        Owner::Embedded => {
            let (node, access) = node().await;
            (node, access, None)
        }
        Owner::ClientSigned => {
            let (node, access, root) = client_signed_node().await;
            (node, access, Some(root))
        }
    };
    let absent = id_of_create(&node, INPUT).await;
    assert!(stored(&node, &format!("{{ IsolationProbe(docID: \"{absent}\") {{ n }} }}"), "IsolationProbe").await.is_empty());
    let b = create(&node, "IsolationProbe", r#"{key: "b", n: 1, m: 1}"#).await;
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
    let observed = race(&access, owner, &decide, async {
        *created.lock().unwrap() = Some(create(&node, "IsolationProbe", INPUT).await);
    })
    .await;
    node.shutdown().await.unwrap();
    (observed, absent, created.into_inner().unwrap().unwrap())
}

#[tokio::test]
async fn a_point_read_of_an_absent_id_conflicts_with_a_concurrent_create_of_it() {
    let (observed, absent, created) = absent_point_read_then_create(Owner::Embedded).await;
    assert_eq!(created, absent, "the create takes the id T1 read");
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

#[tokio::test]
async fn a_client_signed_point_read_of_an_absent_id_conflicts_with_a_concurrent_create_of_it() {
    let (observed, absent, created) = absent_point_read_then_create(Owner::ClientSigned).await;
    assert_eq!(created, absent, "the create takes the id T1 read");
    assert_eq!(observed.attempts, 2, "{observed:?}");
}

/// A transaction that only reads commits whatever committed beside it: T1
/// reads A by id, T2 updates A and commits, T1 commits.
#[tokio::test]
async fn a_read_only_transaction_commits_over_a_concurrent_write_of_what_it_read() {
    let (node, _access) = node().await;
    let a = create(&node, "IsolationProbe", r#"{key: "a", n: 1, m: 1}"#).await;
    let reader = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    let read = format!("{{ IsolationProbe(docID: \"{a}\") {{ n }} }}");
    assert_eq!(rows(&reader.execute(&read).await.unwrap(), "IsolationProbe")[0]["n"], 1);
    write(
        &node,
        format!("mutation {{ update_IsolationProbe(docID: \"{a}\", input: {{n: 5}}) {{ _docID }} }}"),
    )
    .await;
    reader.commit().await.unwrap();
    node.shutdown().await.unwrap();
}
