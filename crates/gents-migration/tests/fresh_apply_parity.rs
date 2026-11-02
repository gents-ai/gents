//! Guard for #1123/#1125: client-authored collections MUST remain fresh-apply
//! compatible. `EmbeddedNode::add_collection` here and a paired client's FFI both
//! ingest SDL into an empty store and converge on the same collection-creation
//! path in the pinned defradb, so the genesis CIDs minted here stand in for
//! what a phone mints. The two apply paths seed their SDL parsers with
//! different known-type sets, which is only guaranteed equivalent while every
//! client-authored SDL is single-type and relation-free — a relation field's
//! CID embeds the target collection's identity, making genesis CIDs depend on
//! the co-registered set.

use gents_migration::{ensure_migrations, CLIENT_AUTHORED_COLLECTIONS};

mod common;
use common::fresh_node;

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
/// SDL uses only scalar field types.
#[test]
fn client_authored_sdls_are_relation_free() {
    const SCALAR_KINDS: &[&str] = &["String", "Int", "Float", "Boolean", "DateTime", "JSON"];
    for &name in CLIENT_AUTHORED_COLLECTIONS {
        for line in current_sdl(name).lines() {
            // GraphQL SDL comments run from `#` to end of line; the `type`
            // header is skipped: its directives may embed `fields: [...]`
            // arguments that are not field declarations.
            let code = line.split('#').next().unwrap_or_default();
            let trimmed = code.trim();
            if trimmed.is_empty() || trimmed.starts_with("type ") {
                continue;
            }
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
    let server_arm = async {
        let server = fresh_node().await;
        ensure_migrations(server.as_ref())
            .await
            .expect("server ensure_migrations");
        server
    };
    let client_arm = async {
        let client = fresh_node().await;
        for &name in CLIENT_AUTHORED_COLLECTIONS {
            client
                .add_collection(current_sdl(name))
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
                 stale — client-authored collections must be re-pinned to the \
                 fresh-apply CID, never chained; see #1123/#1125",
                server_cv.version_id, client_cv.version_id
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

    server.shutdown().await;
    client.shutdown().await;
}
