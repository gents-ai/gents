//! `ProviderAccountUsage` is a baseline collection added after release: an
//! existing store gains it on open, and a build without it still opens.

use defra_node::EmbeddedNode;
use gents_migration::{
    ensure_migrations, ensure_migrations_with_registry, BaselineCollection, Registry,
    DEFAULT_BASELINE, DEFAULT_STEPS,
};
use gents_protocol::schemas::OAUTH_CREDENTIAL_NAME;
use serde_json::Value;

mod common;
use common::fresh_node;

const USAGE: &str = "ProviderAccountUsage";

fn old_baseline() -> &'static [BaselineCollection<'static>] {
    let entries: Vec<_> = DEFAULT_BASELINE
        .iter()
        .copied()
        .filter(|entry| entry.name != USAGE)
        .collect();
    Box::leak(entries.into_boxed_slice())
}

fn old_registry() -> Registry<'static> {
    Registry {
        baseline: old_baseline(),
        steps: DEFAULT_STEPS,
    }
}

async fn seed_sign_in(node: &EmbeddedNode) {
    let response = node
        .execute(
            r#"mutation { create_OAuthCredential(input: {
                credential_id: "chatgpt-codex:did:key:z6MkUsageA"
                node_did: "did:key:z6MkUsageA"
                provider: "chatgpt-codex"
                access_token: "access-TEST"
                refresh_token: "refresh-TEST"
                is_fedramp: false
                access_token_expires_at: "2026-09-01T00:00:00Z"
                last_refresh: "2026-08-31T00:00:00Z"
                enabled: true
            }) { credential_id } }"#,
        )
        .await;
    assert!(!response.has_errors(), "seed: {:?}", response.errors);
}

async fn sign_ins(node: &EmbeddedNode) -> Value {
    let response = node
        .execute("{ OAuthCredential { credential_id node_did access_token enabled } }")
        .await;
    assert!(!response.has_errors(), "read: {:?}", response.errors);
    response.data.expect("data")[OAUTH_CREDENTIAL_NAME].clone()
}

fn oauth_version(node: &EmbeddedNode) -> String {
    node.get_collection(OAUTH_CREDENTIAL_NAME)
        .expect("get OAuthCredential")
        .expect("OAuthCredential present")
        .version_id
}

#[tokio::test]
async fn usage_collection_upgrade_adds_it_and_keeps_sign_ins() {
    let node = fresh_node().await;
    ensure_migrations_with_registry(node.as_ref(), &old_registry())
        .await
        .expect("old build opens a fresh store");
    assert!(node.get_collection(USAGE).expect("get").is_none());
    seed_sign_in(node.as_ref()).await;
    let version = oauth_version(node.as_ref());
    let before = sign_ins(node.as_ref()).await;

    let report = ensure_migrations(node.as_ref()).await.expect("upgrade");
    assert!(node.get_collection(USAGE).expect("get").is_some());
    assert_eq!(report.steps_applied, 0, "{report:?}");
    assert_eq!(oauth_version(node.as_ref()), version);
    assert_eq!(sign_ins(node.as_ref()).await, before);

    node.shutdown().await;
}

#[tokio::test]
async fn usage_collection_upgrade_earlier_build_still_opens() {
    let node = fresh_node().await;
    ensure_migrations_with_registry(node.as_ref(), &old_registry())
        .await
        .expect("old build opens a fresh store");
    ensure_migrations(node.as_ref()).await.expect("upgrade");

    ensure_migrations_with_registry(node.as_ref(), &old_registry())
        .await
        .expect("an earlier build still opens the upgraded store");

    node.shutdown().await;
}
