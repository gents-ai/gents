//! Guard for #1123/#1125: client-authored (conversation-plane) collections
//! MUST remain fresh-apply compatible. A paired client mints its store from
//! a collection's *current* SDL with no server history — one `add_schema`
//! call producing a genesis version — while the server arrives at its active
//! version through the `ensure_migrations` chain replay. Why those can only
//! match while the collection carries no chained steps (the
//! genesis-vs-chain-tip CID mechanism) is documented once, on
//! `CLIENT_AUTHORED_COLLECTIONS` in `src/registry.rs`.
//!
//! The client model: `EmbeddedNode::add_schema` here and a paired client's
//! FFI both ingest SDL into an empty store and converge on the same
//! collection-creation path in the pinned defradb
//! (`create_collections_atomic_with_acp_registration`), so the genesis CIDs
//! minted here stand in for what a phone mints. The two apply paths seed
//! their SDL parsers with different known-type sets, which is only
//! guaranteed equivalent while every client-authored SDL is single-type and
//! relation-free — a relation field's CID embeds the target collection's
//! identity, making genesis CIDs depend on the co-registered set.
//! `client_authored_sdls_are_relation_free` pins that precondition.
//!
//! Limitation: the parity comparison is by version CID, so it cannot detect
//! server-side `PatchInPlace` divergence (index/policy/embedding patches
//! keep the CID). The static step guard in `baseline_ensure.rs`
//! (`default_baseline_matches_ordered_protocol_catalog`) closes that gap by
//! rejecting DEFAULT_STEPS entries of any kind for these collections.

use gents_migration::{ensure_migrations, CLIENT_AUTHORED_COLLECTIONS};

mod common;
use common::fresh_node;

/// The live SDL a fresh client mints its store from, looked up by position
/// in the protocol catalog (the same order-aligned name/SDL arrays
/// `baseline_ensure.rs` reconciles) so that a collection added to
/// `CLIENT_AUTHORED_COLLECTIONS` is covered here automatically.
fn current_sdl(name: &str) -> &'static str {
    gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(gents_protocol::schemas::RUNTIME_ALL.iter().copied())
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(gents_protocol::schemas::ALL.iter().copied()),
        )
        .find_map(|(catalog_name, sdl)| (catalog_name == name).then_some(sdl))
        .unwrap_or_else(|| panic!("{name} is not in the protocol schema catalog"))
}

/// Precondition of the client model (see module doc): every client-authored
/// SDL uses only scalar field types. A relation field would make the genesis
/// CID depend on which other types are co-registered, and the per-collection
/// apply below would no longer model a client's batch apply.
#[test]
fn client_authored_sdls_are_relation_free() {
    const SCALAR_KINDS: &[&str] = &["String", "Int", "Float", "Boolean", "DateTime"];
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        for line in current_sdl(name).lines() {
            // GraphQL SDL comments run from `#` to end of line; a colon
            // inside one is not a field type.
            let code = line.split('#').next().unwrap_or_default();
            let Some((_, after_colon)) = code.split_once(": ") else {
                continue;
            };
            let kind = after_colon
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c| matches!(c, '[' | ']' | '!'));
            assert!(
                SCALAR_KINDS.contains(&kind),
                "{name} field `{}` uses non-scalar type {kind:?}; client-authored \
                 collections must stay relation-free (or, if DefraDB gained a new scalar \
                 kind, extend SCALAR_KINDS) — see the module doc",
                line.trim()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_authored_collections_stay_fresh_apply_compatible() {
    // The two arms are independent until the comparison; run them
    // concurrently — this is the crate's most expensive test.
    let server_arm = async {
        // Node A: the real server boot path — full ensure_migrations chain
        // replay (baseline registration + every DEFAULT_STEPS entry).
        let server = fresh_node().await;
        ensure_migrations(server.as_ref())
            .await
            .expect("server ensure_migrations");
        server
    };
    let client_arm = async {
        // Node B: a fresh node standing in for a paired client, applying each
        // client-authored collection's CURRENT SDL directly — the single-call
        // add_schema genesis path a paired client takes (see module doc).
        let client = fresh_node().await;
        for &name in CLIENT_AUTHORED_COLLECTIONS {
            client
                .add_schema(current_sdl(name))
                .await
                .unwrap_or_else(|error| panic!("client fresh-apply {name}: {error}"));
        }
        client
    };
    let (server, client) = tokio::join!(server_arm, client_arm);

    let mut mismatches = Vec::new();
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        let server_cv = server
            .get_collection(name)
            .expect("server get_collection")
            .unwrap_or_else(|| panic!("server missing {name}"));
        let client_cv = client
            .get_collection(name)
            .expect("client get_collection")
            .unwrap_or_else(|| panic!("client missing {name}"));
        if server_cv.version_id != client_cv.version_id {
            mismatches.push(format!(
                "{name}: server active version {} != client fresh-apply version {} — \
                 collection gained a post-baseline migration step or its baseline pin is \
                 stale — conversation-plane collections must be re-pinned to the \
                 fresh-apply CID, never chained; see #1123/#1125",
                server_cv.version_id, client_cv.version_id
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

    server.shutdown().await;
    client.shutdown().await;
}
