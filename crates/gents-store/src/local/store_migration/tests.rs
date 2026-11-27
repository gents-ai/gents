use std::path::{Path, PathBuf};
use std::sync::Arc;

use defra_core::RequestContext;
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use gents_protocol::schemas::{self, BuiltInPolicy, HomePolicyIds};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::graphql::created_doc_id;
use crate::identity::{KeyIdentity, NodeIdentity};
use crate::local::backend_protection::{
    refuse_other_lineage, write_provenance_marker_for_lineage, BackendProtectionError,
    FreshBackendStore, ProtectedBackendStore, LINEAGE_VERSION, PROVENANCE_MARKER,
};
use crate::local::document_acp::DocumentAcp;
use crate::local::home;
use crate::local::storage_backend::{incompatible_store, DEFAULT_BLOCK_CACHE_BYTES};
use crate::local::store_compat::{self, check_home_store, StoreCompatibility};
use crate::local::store_key::{persistent_builder, StoreKey};
use crate::schema::catalog::{
    self, BaselineCollection, Catalog, OrdinaryIndex, OrdinaryIndexField,
};

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn store_bytes(root: &Path) -> TestResult<Vec<(PathBuf, [u8; 32])>> {
    let mut contents = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            contents.extend(store_bytes(&entry.path())?);
        } else {
            contents.push((entry.path(), Sha256::digest(fs::read(entry.path())?).into()));
        }
    }
    contents.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(contents)
}

struct FreshTestHome {
    _dir: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    key: StoreKey,
    node: Arc<dyn NodeIdentity>,
    owner: identity::Did,
}

async fn create_fresh_1_0_home(key_seed: u8) -> TestResult<FreshTestHome> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().to_path_buf();
    let data = home.join("data");
    let (lock_home, lock_data) = (home.clone(), data.clone());
    let lock = tokio::task::spawn_blocking(move || -> TestResult<home::StoreLock> {
        fs::create_dir_all(&lock_data)?;
        Ok(home::lock_store(&lock_home, &lock_data)?)
    })
    .await??;
    let key = StoreKey::from_host(&[key_seed; 32])?;
    let node_key_path = home.join("node.key");
    let node: Arc<dyn NodeIdentity> =
        tokio::task::spawn_blocking(move || -> TestResult<Arc<dyn NodeIdentity>> {
            Ok(Arc::new(KeyIdentity::load_or_create(node_key_path, None)?))
        })
        .await??;
    let owner_key_path = home.join("owner.key");
    let owner_identity: Arc<dyn NodeIdentity> =
        tokio::task::spawn_blocking(move || -> TestResult<Arc<dyn NodeIdentity>> {
            Ok(Arc::new(KeyIdentity::load_or_create(owner_key_path, None)?))
        })
        .await??;
    let owner = identity::Did::new(owner_identity.did())?;
    FreshBackendStore::observe(&data, &lock)
        .await?
        .initialize(&key, &node, &owner, DocumentAcp::Local)
        .await?;
    drop(lock);
    Ok(FreshTestHome {
        _dir: dir,
        home,
        data,
        key,
        node,
        owner,
    })
}

async fn exec_as(
    node: &defra_node::Node,
    actor: &identity::Did,
    query: &str,
) -> TestResult<serde_json::Value> {
    let request = defra_node::QueryRequest::new(query)
        .with_identity(Some(actor.clone()))
        .with_context(actor_context(actor.as_str()));
    let response = node.execute_request(request).await;
    if response.has_errors() {
        return Err(format!("GraphQL errors for {actor}: {:?}", response.errors).into());
    }
    Ok(serde_json::json!({ "data": response.data }))
}

async fn create_as(
    node: &defra_node::Node,
    actor: &identity::Did,
    collection: &str,
    mutation: &str,
) -> TestResult<String> {
    let response = exec_as(node, actor, mutation).await?;
    Ok(created_doc_id(&response, collection)?)
}

async fn create_actor_did(dir: &Path, name: &str) -> TestResult<identity::Did> {
    let key_path = dir.join(format!("{name}.key"));
    tokio::task::spawn_blocking(move || -> TestResult<identity::Did> {
        let key = KeyIdentity::load_or_create(key_path, None)?;
        Ok(identity::Did::new(key.did())?)
    })
    .await?
}

async fn open_encrypted_node(
    data: &Path,
    key: &StoreKey,
    node_did: &str,
) -> TestResult<Arc<defra_node::Node>> {
    let node = persistent_builder(data, key, DEFAULT_BLOCK_CACHE_BYTES)?
        .with_node_identity_did(node_did.to_owned())
        .with_node_acp_enabled()
        .build()
        .await?;
    Ok(Arc::new(node))
}

fn actor_context(did: &str) -> RequestContext {
    RequestContext::default().with_identity(Some(did.to_owned()))
}

async fn grant_read_nac(
    node: &defra_node::Node,
    owner: &identity::Did,
    actor: &identity::Did,
) -> TestResult {
    let owner_ctx = actor_context(owner.as_str());
    for permission in ["get-collection", "read-document"] {
        node.add_nac_actor_relationship_as(&owner_ctx, permission, actor.as_str())
            .await?;
    }
    Ok(())
}

async fn grant_node_runtime_nac(
    node: &defra_node::Node,
    owner: &identity::Did,
    node_did: &str,
) -> TestResult {
    let owner_ctx = actor_context(owner.as_str());
    for permission in crate::identity::node_runtime_permissions() {
        node.add_nac_actor_relationship_as(&owner_ctx, permission.as_str(), node_did)
            .await?;
    }
    Ok(())
}

#[test]
fn store_migration_contract_matches_lean_model() {
    use gents_lean_vocab::families;
    families::store_migration_open_disposition::replay(|input| {
        open_disposition(
            input.lineage,
            input.manifest,
            input.migratable_from,
            input.before_lock,
        )
    });
    families::store_migration_change_rule::replay(|input| {
        let rule = change_rule(input.kind);
        families::store_migration_change_rule::ChangeRule {
            handling: rule.handling,
            signer: rule.signer,
            bumps_lineage: rule.bumps_lineage,
        }
    });
    families::store_migration_migration_step::replay(|input| {
        migration_step(input.phase, input.source, input.stage, input.retired)
    });
    families::store_migration_may_finish::replay(|input| {
        may_finish(input.phase, input.target_accepted)
    });
    families::store_migration_policy_transition::replay(|input| {
        let plan = policy_transition(
            input.policy_id_changed,
            input.resource_changed,
            input.same_policy_id_rules_changed,
            input.provider,
        );
        families::store_migration_policy_transition::PolicyTransitionPlan {
            policy_registration: plan.policy_registration,
            rebind_collections: plan.rebind_collections,
            re_register_documents: plan.re_register_documents,
            rebuild_staged_store: plan.rebuild_staged_store,
            signer: plan.signer,
        }
    });
}

#[tokio::test]
async fn fresh_1_0_store_writes_typed_manifest_and_refuses_incompatible_homes_byte_identically(
) -> TestResult {
    let fixture = create_fresh_1_0_home(11).await?;
    let expected_ids = HomePolicyIds::node_store();
    let manifest = read_manifest(&fixture.data)?.expect("1.0 store manifest written");
    assert_eq!(manifest.contract_version, STORE_CONTRACT_VERSION);
    assert_eq!(manifest.lineage, LINEAGE_VERSION);
    assert_eq!(manifest.storage_backend, STORAGE_BACKEND_REGOLITH);
    assert_eq!(manifest.document_acp, DocumentAcp::Local);
    assert_eq!(
        manifest.policy_ids,
        ManifestPolicyIds::from_home_policy_ids(expected_ids)
    );
    assert_eq!(
        manifest.schema_fingerprint,
        store_compat::fingerprint(expected_ids)
    );
    assert_eq!(manifest.migrated_from, None);

    refuse_other_lineage(&fixture.data)?;
    assert_eq!(
        check_home_store(&fixture.home).await?,
        StoreCompatibility::Compatible
    );

    let original_manifest_bytes = fs::read(manifest_path(&fixture.data))?;
    let original_marker_bytes = fs::read(fixture.data.join(PROVENANCE_MARKER))?;

    let assert_refused_byte_identical =
        |home: &Path, data: &Path, expected_kind: IncompatibleStoreKind, expected_older: bool| {
            let home = home.to_path_buf();
            let data = data.to_path_buf();
            async move {
                let before = store_bytes(&home)?;
                let pre_lock =
                    refuse_other_lineage(&data).expect_err("incompatible store refused pre-lock");
                let pre_lock_store =
                    incompatible_store(&pre_lock, &data).expect("typed pre-lock refusal");
                assert_eq!(pre_lock_store.kind, expected_kind);
                assert_eq!(pre_lock_store.kind.is_older(), expected_older);
                assert_eq!(store_bytes(&home)?, before, "pre-lock refusal untouched");

                let verdict = check_home_store(&home).await?;
                match verdict {
                    StoreCompatibility::Refused(store) => {
                        assert_eq!(store.kind, expected_kind);
                        assert_eq!(store.kind.is_older(), expected_older);
                    }
                    other => panic!("expected refusal {expected_kind:?}, got {other:?}"),
                }
                assert_eq!(store_bytes(&home)?, before, "check_home_store untouched");
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            }
        };

    // 1. Unknown / unparseable lineage markers fail closed as newer (never offered for deletion).
    for unknown in [".gents-private-backend-vx", ".gents-private-backend-v03"] {
        fs::remove_file(fixture.data.join(PROVENANCE_MARKER))?;
        fs::write(fixture.data.join(unknown), &original_marker_bytes)?;
        assert_refused_byte_identical(
            &fixture.home,
            &fixture.data,
            IncompatibleStoreKind::ForeignVersion,
            false,
        )
        .await?;
        fs::remove_file(fixture.data.join(unknown))?;
        fs::write(fixture.data.join(PROVENANCE_MARKER), &original_marker_bytes)?;
    }

    // 2. Older lineage marker + older manifest refused as older, byte-identical.
    let mut older_manifest = manifest.clone();
    older_manifest.lineage = LINEAGE_VERSION - 1;
    write_manifest(&fixture.data, &older_manifest)?;
    fs::rename(
        fixture.data.join(PROVENANCE_MARKER),
        fixture.data.join(".gents-private-backend-v2"),
    )?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::UnknownLineage,
        true,
    )
    .await?;
    fs::rename(
        fixture.data.join(".gents-private-backend-v2"),
        fixture.data.join(PROVENANCE_MARKER),
    )?;
    fs::write(manifest_path(&fixture.data), &original_manifest_bytes)?;

    // 3. Newer lineage marker refused as ForeignVersion (never offered for deletion).
    fs::rename(
        fixture.data.join(PROVENANCE_MARKER),
        fixture.data.join(".gents-private-backend-v4"),
    )?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::ForeignVersion,
        false,
    )
    .await?;
    fs::rename(
        fixture.data.join(".gents-private-backend-v4"),
        fixture.data.join(PROVENANCE_MARKER),
    )?;

    // 4. Newer manifest contract version with unknown future fields fails closed as ForeignVersion.
    fs::write(
        manifest_path(&fixture.data),
        serde_json::to_vec(&serde_json::json!({
            "contract_version": STORE_CONTRACT_VERSION + 1,
            "lineage": LINEAGE_VERSION,
            "future_field": "kept-intact"
        }))?,
    )?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::ForeignVersion,
        false,
    )
    .await?;

    // 5. Corrupt JSON manifest refused as UnsupportedOrCorrupt, byte-identical.
    fs::write(manifest_path(&fixture.data), b"{not valid json")?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::UnsupportedOrCorrupt,
        true,
    )
    .await?;

    // 6. Non-Regolith storage backend in manifest refused as UnsupportedOrCorrupt.
    let mut foreign_backend = manifest.clone();
    foreign_backend.storage_backend = "other-kv".to_owned();
    write_manifest(&fixture.data, &foreign_backend)?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::UnsupportedOrCorrupt,
        true,
    )
    .await?;

    // 7. Mismatched manifest lineage beside current marker refused as UnsupportedOrCorrupt.
    write_manifest(&fixture.data, &older_manifest)?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::UnsupportedOrCorrupt,
        true,
    )
    .await?;

    // 8. Missing manifest beside current marker refused as UnknownLineage, byte-identical.
    fs::remove_file(manifest_path(&fixture.data))?;
    assert_refused_byte_identical(
        &fixture.home,
        &fixture.data,
        IncompatibleStoreKind::UnknownLineage,
        true,
    )
    .await?;
    let lock = home::lock_store(&fixture.home, &fixture.data)?;
    let opened = ProtectedBackendStore::open(&fixture.data, &fixture.key, &lock).await;
    assert!(matches!(
        opened,
        Err(BackendProtectionError::MissingProvenance)
    ));
    drop(lock);

    Ok(())
}

#[tokio::test]
async fn synthetic_boot_addition_registers_new_collection_and_index_in_place() -> TestResult {
    assert_eq!(
        change_rule(ChangeKind::NewCollection),
        ChangeRule {
            handling: ChangeHandling::InPlaceOnBoot,
            signer: MigrationSigner::Node,
            bumps_lineage: false,
        }
    );
    assert_eq!(
        change_rule(ChangeKind::NewOrdinaryIndex),
        ChangeRule {
            handling: ChangeHandling::InPlaceOnBoot,
            signer: MigrationSigner::Node,
            bumps_lineage: false,
        }
    );

    let fixture = create_fresh_1_0_home(21).await?;
    let node = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    let _node_doc = create_as(
        &node,
        &fixture.owner,
        "Node",
        &format!(
            r#"mutation {{ create_Node(input: {{ node_did: "{}", display_name: "Home One", enabled: true }}) {{ _docID }} }}"#,
            fixture.node.did()
        ),
    )
    .await?;
    node.shutdown().await?;
    drop(node);

    // Boot-time addition: add a brand-new collection and ordinary index in place as Node.
    let ids = HomePolicyIds::node_store();
    const NEW_COLLECTION_NAME: &str = "SyntheticReleaseNote";
    const NEW_COLLECTION_SDL: &str = r#"type SyntheticReleaseNote @policy(id: "$household", resource: "configuration") {
    note_id: String @index(unique: true)
    tag: String
    body: String
}"#;
    let mut entries = catalog::baseline(ids).to_vec();
    entries.push(BaselineCollection {
        name: NEW_COLLECTION_NAME,
        sdl: NEW_COLLECTION_SDL,
        expected_version: None,
    });
    let mut indexes = catalog::ORDINARY_INDEXES.to_vec();
    const TAG_FIELDS: &[OrdinaryIndexField<'static>] = &[OrdinaryIndexField {
        name: "tag",
        descending: false,
    }];
    indexes.push(OrdinaryIndex {
        collection: NEW_COLLECTION_NAME,
        name: "idx_synthetic_release_note_tag",
        fields: TAG_FIELDS,
    });
    let extended_catalog = Catalog {
        baseline: &entries,
        ordinary_indexes: &indexes,
    };

    let reopened = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    catalog::ensure(&reopened, &extended_catalog).await?;

    let retained = exec_as(
        &reopened,
        &fixture.owner,
        "{ Node { node_did display_name } }",
    )
    .await?;
    let retained_rows = gents_protocol::graphql::graphql_rows_from_response(&retained, "Node");
    assert_eq!(retained_rows.len(), 1, "{retained}");
    assert_eq!(retained_rows[0]["display_name"], "Home One");

    let _note_doc = create_as(
        &reopened,
        &fixture.owner,
        "SyntheticReleaseNote",
        r#"mutation { create_SyntheticReleaseNote(input: { note_id: "n1", tag: "v1.1", body: "boot-added" }) { _docID note_id tag body } }"#,
    )
    .await?;
    let queried = exec_as(
        &reopened,
        &fixture.owner,
        r#"{ SyntheticReleaseNote(filter: { tag: { _eq: "v1.1" } }) { note_id body } }"#,
    )
    .await?;
    let note_rows =
        gents_protocol::graphql::graphql_rows_from_response(&queried, "SyntheticReleaseNote");
    assert_eq!(note_rows.len(), 1, "{queried}");
    assert_eq!(note_rows[0]["body"], "boot-added");

    let manifest = read_manifest(&fixture.data)?.expect("manifest retained");
    assert_eq!(manifest.lineage, LINEAGE_VERSION);
    assert_eq!(manifest.migrated_from, None);
    reopened.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn synthetic_additive_nullable_schema_migration_preserves_docs_and_adds_field() -> TestResult
{
    let rule = change_rule(ChangeKind::AdditiveNullableField);
    assert_eq!(rule.handling, ChangeHandling::StagedMigration);
    assert_eq!(rule.signer, MigrationSigner::Node);
    assert!(rule.bumps_lineage);

    let fixture = create_fresh_1_0_home(31).await?;
    let node = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    let _node_doc = create_as(
        &node,
        &fixture.owner,
        "Node",
        &format!(
            r#"mutation {{ create_Node(input: {{ node_did: "{}", display_name: "Before Migration", enabled: true }}) {{ _docID }} }}"#,
            fixture.node.did()
        ),
    )
    .await?;
    node.shutdown().await?;
    drop(node);

    let target_lineage = LINEAGE_VERSION + 1;
    let ids = HomePolicyIds::node_store();
    begin_staged_migration(
        &fixture.data,
        LINEAGE_VERSION,
        target_lineage,
        ChangeKind::AdditiveNullableField,
    )?;

    let key = &fixture.key;
    let node_did = fixture.node.did().to_owned();
    let owner = fixture.owner.clone();
    run_staged_migration::<_, _, Box<dyn std::error::Error + Send + Sync>>(
        &fixture.data,
        |source, stage| {
            let node_did = node_did.clone();
            let owner = owner.clone();
            async move {
                copy_store_tree(&source, &stage)?;
                let staged = open_encrypted_node(&stage, key, &node_did).await?;
                let node_ctx = actor_context(&node_did);
                let patch = r#"[{"op":"add","path":"/Node/Fields/-","value":{"Name":"synthetic_v2_note","Kind":"String"}}]"#;
                staged.patch_collection_as(&node_ctx, patch, None).await?;
                let read = exec_as(
                    &staged,
                    &owner,
                    "{ Node { display_name synthetic_v2_note } }",
                )
                .await?;
                let rows = gents_protocol::graphql::graphql_rows_from_response(&read, "Node");
                assert_eq!(rows.len(), 1, "{read}");
                assert_eq!(rows[0]["display_name"], "Before Migration");
                assert!(rows[0]["synthetic_v2_note"].is_null());
                staged.shutdown().await?;
                drop(staged);

                fs::remove_file(stage.join(PROVENANCE_MARKER))?;
                let migrated_manifest = StoreManifest::for_migrated_store(
                    LINEAGE_VERSION,
                    target_lineage,
                    DocumentAcp::Local,
                    ManifestPolicyIds::from_home_policy_ids(ids),
                    format!("{}-v4-additive", store_compat::fingerprint(ids)),
                );
                write_manifest(&stage, &migrated_manifest)?;
                write_provenance_marker_for_lineage(
                    &stage,
                    key,
                    target_lineage,
                    storage::backends::DurabilityMode::Immediate,
                )
                .await?;
                Ok(())
            }
        },
    )
    .await?;

    // The promoted store opens on v4 and is refused as newer by a 1.0 (v3) build.
    assert_eq!(
        inspect_store_disposition(&fixture.data, target_lineage, false, false)?,
        OpenDisposition::Open
    );
    assert_eq!(
        inspect_store_disposition(&fixture.data, LINEAGE_VERSION, false, true)?,
        OpenDisposition::RefuseNewer
    );

    let promoted = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    let read = exec_as(
        &promoted,
        &fixture.owner,
        "{ Node { display_name synthetic_v2_note } }",
    )
    .await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&read, "Node");
    assert_eq!(rows.len(), 1, "{read}");
    assert_eq!(rows[0]["display_name"], "Before Migration");
    assert!(rows[0]["synthetic_v2_note"].is_null());

    let _new_doc = create_as(
        &promoted,
        &fixture.owner,
        "Node",
        r#"mutation { create_Node(input: { node_did: "did:key:z6Mkv4Peer", display_name: "After Migration", enabled: true, synthetic_v2_note: "migrated-ok" }) { _docID synthetic_v2_note } }"#,
    )
    .await?;
    let read_new = exec_as(
        &promoted,
        &fixture.owner,
        r#"{ Node(filter: { node_did: { _eq: "did:key:z6Mkv4Peer" } }) { synthetic_v2_note } }"#,
    )
    .await?;
    let new_rows = gents_protocol::graphql::graphql_rows_from_response(&read_new, "Node");
    assert_eq!(new_rows.len(), 1, "{read_new}");
    assert_eq!(new_rows[0]["synthetic_v2_note"], "migrated-ok");
    promoted.shutdown().await?;

    finish_staged_migration(&fixture.data, true)?;
    assert!(!retired_path(&fixture.data).exists());
    assert!(load_journal(&fixture.data)?.is_none());
    let final_manifest = read_manifest(&fixture.data)?.expect("migrated manifest");
    assert_eq!(final_manifest.lineage, target_lineage);
    assert_eq!(final_manifest.migrated_from, Some(LINEAGE_VERSION));
    Ok(())
}

#[tokio::test]
async fn synthetic_policy_id_migration_carries_documents_and_acp_across_policy_change() -> TestResult
{
    let plan = policy_transition(true, false, false, AcpProvider::Local);
    assert_eq!(plan.policy_registration, PolicyRegistration::NodeCounter);
    assert!(plan.rebind_collections);
    assert!(plan.re_register_documents);
    assert!(plan.rebuild_staged_store);
    assert_eq!(plan.signer, MigrationSigner::User);

    let fixture = create_fresh_1_0_home(41).await?;
    let authorized_admin = create_actor_did(&fixture.home, "authorized-admin").await?;
    let outsider = create_actor_did(&fixture.home, "outsider").await?;
    let old_ids = HomePolicyIds::node_store();
    let old_policy_id = old_ids.id(BuiltInPolicy::Household);

    // Populate the 1.0 store with User-owned Node + child Agent + ACP relations.
    let source_node = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    grant_read_nac(&source_node, &fixture.owner, &authorized_admin).await?;
    grant_read_nac(&source_node, &fixture.owner, &outsider).await?;

    let old_node_doc = create_as(
        &source_node,
        &fixture.owner,
        "Node",
        &format!(
            r#"mutation {{ create_Node(input: {{ node_did: "{}", display_name: "Household Root", enabled: true }}) {{ _docID }} }}"#,
            fixture.node.did()
        ),
    )
    .await?;
    let old_agent_doc = create_as(
        &source_node,
        &fixture.owner,
        "Agent",
        &format!(
            r#"mutation {{ create_Agent(input: {{ agent_did: "did:key:z6MkAgentOne", creation_nonce: "nonce-1", node_did: "{}", display_name: "Engineer", enabled: true }}) {{ _docID }} }}"#,
            fixture.node.did()
        ),
    )
    .await?;

    let acp = source_node.document_acp();
    acp.add_actor_relationship(
        &fixture.owner,
        &authorized_admin,
        old_policy_id,
        "nodes",
        &old_node_doc,
        "administrator",
        &[],
    )
    .await?;
    acp.add_relationship(
        &fixture.owner,
        acp::Subject::entity_set("nodes", &old_node_doc, ""),
        old_policy_id,
        "agents",
        &old_agent_doc,
        "parent",
        &[],
    )
    .await?;

    // Confirm 1.0 ACP: authorized_admin reads Agent via parent->read; outsider sees nothing.
    let admin_before = exec_as(
        &source_node,
        &authorized_admin,
        "{ Agent { agent_did display_name } }",
    )
    .await?;
    let admin_before_rows =
        gents_protocol::graphql::graphql_rows_from_response(&admin_before, "Agent");
    assert_eq!(admin_before_rows.len(), 1, "{admin_before}");
    assert_eq!(admin_before_rows[0]["display_name"], "Engineer");
    let outsider_before = exec_as(
        &source_node,
        &outsider,
        "{ Agent { agent_did display_name } }",
    )
    .await?;
    assert_eq!(
        gents_protocol::graphql::graphql_rows_from_response(&outsider_before, "Agent").len(),
        0
    );
    drop(acp);
    source_node.shutdown().await?;
    drop(source_node);

    // Synthesize a post-1.0 Household policy whose YAML change yields a new policy ID.
    let new_household_policy = format!(
        "{}\n  - name: synthetic_v4_extension\n    relations:\n      - name: parent\n        types: [nodes]\n    permissions:\n      - name: read\n        expr: parent->read\n",
        schemas::AGENT_OWNERSHIP_POLICY
    );
    let target_lineage = LINEAGE_VERSION + 1;
    begin_staged_migration(
        &fixture.data,
        LINEAGE_VERSION,
        target_lineage,
        ChangeKind::PolicyIdOrRuleChange,
    )?;

    let key = &fixture.key;
    let node_did = fixture.node.did().to_owned();
    let owner = fixture.owner.clone();
    let admin = authorized_admin.clone();
    let outsider_did = outsider.clone();
    run_staged_migration::<_, _, Box<dyn std::error::Error + Send + Sync>>(
        &fixture.data,
        |source, stage| {
            let node_did = node_did.clone();
            let owner = owner.clone();
            let admin = admin.clone();
            let outsider_did = outsider_did.clone();
            let new_household_policy = new_household_policy.clone();
            async move {
                // Export source documents as User (NAC owner).
                let src = open_encrypted_node(&source, key, &node_did).await?;
                let src_nodes_resp =
                    exec_as(&src, &owner, "{ Node { node_did display_name enabled } }").await?;
                let src_nodes =
                    gents_protocol::graphql::graphql_rows_from_response(&src_nodes_resp, "Node");
                assert_eq!(src_nodes.len(), 1, "{src_nodes_resp}");
                let src_agents_resp = exec_as(
                    &src,
                    &owner,
                    "{ Agent { agent_did creation_nonce node_did display_name enabled } }",
                )
                .await?;
                let src_agents =
                    gents_protocol::graphql::graphql_rows_from_response(&src_agents_resp, "Agent");
                assert_eq!(src_agents.len(), 1, "{src_agents_resp}");
                src.shutdown().await?;
                drop(src);

                // Rebuild fresh encrypted Regolith stage under the new policy ID.
                let staged = Arc::new(
                    persistent_builder(&stage, key, DEFAULT_BLOCK_CACHE_BYTES)?
                        .with_node_identity_did(node_did.clone())
                        .with_node_acp_owner(owner.as_str())
                        .build()
                        .await?,
                );
                grant_node_runtime_nac(&staged, &owner, &node_did).await?;
                grant_read_nac(&staged, &owner, &admin).await?;
                grant_read_nac(&staged, &owner, &outsider_did).await?;

                let new_policy_id = staged
                    .add_dac_policy_with_counter_as(
                        &actor_context(&node_did),
                        &new_household_policy,
                        0,
                    )
                    .await?;
                assert_ne!(
                    new_policy_id, old_policy_id,
                    "synthetic policy change must produce a distinct policy ID"
                );

                let render_new = |sdl: &str| {
                    sdl.replace(
                        &format!("id: \"{}\"", BuiltInPolicy::Household.placeholder()),
                        &format!("id: \"{new_policy_id}\""),
                    )
                };
                staged
                    .add_collection(&format!(
                        "{}\n{}",
                        render_new(schemas::NODE),
                        render_new(schemas::AGENT)
                    ))
                    .await?;

                let node_row = &src_nodes[0];
                let new_node_doc = create_as(
                    &staged,
                    &owner,
                    "Node",
                    &format!(
                        r#"mutation {{ create_Node(input: {{ node_did: "{}", display_name: "{}", enabled: {} }}) {{ _docID }} }}"#,
                        node_row["node_did"].as_str().unwrap(),
                        node_row["display_name"].as_str().unwrap(),
                        node_row["enabled"].as_bool().unwrap()
                    ),
                )
                .await?;

                let agent_row = &src_agents[0];
                let new_agent_doc = create_as(
                    &staged,
                    &owner,
                    "Agent",
                    &format!(
                        r#"mutation {{ create_Agent(input: {{ agent_did: "{}", creation_nonce: "{}", node_did: "{}", display_name: "{}", enabled: {} }}) {{ _docID }} }}"#,
                        agent_row["agent_did"].as_str().unwrap(),
                        agent_row["creation_nonce"].as_str().unwrap(),
                        agent_row["node_did"].as_str().unwrap(),
                        agent_row["display_name"].as_str().unwrap(),
                        agent_row["enabled"].as_bool().unwrap()
                    ),
                )
                .await?;

                // Re-register ACP relationships under the new policy ID.
                let stage_acp = staged.document_acp();
                stage_acp
                    .add_actor_relationship(
                        &owner,
                        &admin,
                        &new_policy_id,
                        "nodes",
                        &new_node_doc,
                        "administrator",
                        &[],
                    )
                    .await?;
                stage_acp
                    .add_relationship(
                        &owner,
                        acp::Subject::entity_set("nodes", &new_node_doc, ""),
                        &new_policy_id,
                        "agents",
                        &new_agent_doc,
                        "parent",
                        &[],
                    )
                    .await?;

                // Verify staged store before retiring source.
                let staged_admin = exec_as(&staged, &admin, "{ Agent { display_name } }").await?;
                let staged_admin_rows =
                    gents_protocol::graphql::graphql_rows_from_response(&staged_admin, "Agent");
                assert_eq!(staged_admin_rows.len(), 1, "{staged_admin}");
                assert_eq!(staged_admin_rows[0]["display_name"], "Engineer");
                let staged_outsider =
                    exec_as(&staged, &outsider_did, "{ Agent { display_name } }").await?;
                assert_eq!(
                    gents_protocol::graphql::graphql_rows_from_response(&staged_outsider, "Agent")
                        .len(),
                    0
                );
                drop(stage_acp);
                staged.shutdown().await?;
                drop(staged);

                let mut migrated_ids = ManifestPolicyIds::from_home_policy_ids(old_ids);
                migrated_ids.household = new_policy_id;
                let manifest = StoreManifest::for_migrated_store(
                    LINEAGE_VERSION,
                    target_lineage,
                    DocumentAcp::Local,
                    migrated_ids,
                    "synthetic-v4-policy-fingerprint".to_owned(),
                );
                write_manifest(&stage, &manifest)?;
                write_provenance_marker_for_lineage(
                    &stage,
                    key,
                    target_lineage,
                    storage::backends::DurabilityMode::Immediate,
                )
                .await?;
                Ok(())
            }
        },
    )
    .await?;

    // Verify promoted store enforces the new policy ID for both authorized and unauthorized reads.
    let manifest = read_manifest(&fixture.data)?.expect("v4 manifest");
    assert_ne!(manifest.policy_ids.household, old_policy_id);
    assert_eq!(manifest.migrated_from, Some(LINEAGE_VERSION));

    let promoted = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    let agent_collection = promoted.get_collection(schemas::AGENT_NAME)?.unwrap();
    assert_eq!(
        agent_collection.policy.as_ref().unwrap().id,
        manifest.policy_ids.household
    );
    let admin_after = exec_as(
        &promoted,
        &authorized_admin,
        "{ Agent { agent_did display_name } }",
    )
    .await?;
    let admin_after_rows =
        gents_protocol::graphql::graphql_rows_from_response(&admin_after, "Agent");
    assert_eq!(admin_after_rows.len(), 1, "{admin_after}");
    assert_eq!(admin_after_rows[0]["display_name"], "Engineer");
    let outsider_after =
        exec_as(&promoted, &outsider, "{ Agent { agent_did display_name } }").await?;
    assert_eq!(
        gents_protocol::graphql::graphql_rows_from_response(&outsider_after, "Agent").len(),
        0
    );
    promoted.shutdown().await?;

    finish_staged_migration(&fixture.data, true)?;
    Ok(())
}

#[tokio::test]
async fn synthetic_ownership_and_resource_migration_rebinds_collection_and_enforces_new_access(
) -> TestResult {
    let plan = policy_transition(false, true, false, AcpProvider::Local);
    assert_eq!(plan.policy_registration, PolicyRegistration::None);
    assert!(plan.rebind_collections);
    assert!(plan.re_register_documents);
    assert!(plan.rebuild_staged_store);
    assert_eq!(plan.signer, MigrationSigner::User);

    let fixture = create_fresh_1_0_home(51).await?;
    let delegated_reader = create_actor_did(&fixture.home, "delegated-reader").await?;
    let ids = HomePolicyIds::node_store();
    let policy_id = ids.id(BuiltInPolicy::Household);

    let source_node = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    grant_read_nac(&source_node, &fixture.owner, &delegated_reader).await?;

    let old_skill_doc = create_as(
        &source_node,
        &fixture.owner,
        "Skill",
        &format!(
            r#"mutation {{ create_Skill(input: {{ skill_id: "sk-1", node_did: "{}", name: "deploy", instructions: "keep-secret", enabled: true }}) {{ _docID }} }}"#,
            fixture.node.did()
        ),
    )
    .await?;

    // Under 1.0 `configuration` resource, `delegated_reader` holds `reader` and can read `Skill`.
    source_node
        .document_acp()
        .add_actor_relationship(
            &fixture.owner,
            &delegated_reader,
            policy_id,
            "configuration",
            &old_skill_doc,
            "reader",
            &[],
        )
        .await?;
    let reader_before = exec_as(
        &source_node,
        &delegated_reader,
        "{ Skill { skill_id instructions } }",
    )
    .await?;
    let reader_before_rows =
        gents_protocol::graphql::graphql_rows_from_response(&reader_before, "Skill");
    assert_eq!(reader_before_rows.len(), 1, "{reader_before}");
    assert_eq!(reader_before_rows[0]["instructions"], "keep-secret");
    source_node.shutdown().await?;
    drop(source_node);

    // Migrate `Skill` from `configuration` to `secret_configuration` (User-signed rebuild).
    let target_lineage = LINEAGE_VERSION + 1;
    begin_staged_migration(
        &fixture.data,
        LINEAGE_VERSION,
        target_lineage,
        ChangeKind::DocumentOwnershipOrResourceChange,
    )?;

    let key = &fixture.key;
    let node_did = fixture.node.did().to_owned();
    let owner = fixture.owner.clone();
    let reader = delegated_reader.clone();
    run_staged_migration::<_, _, Box<dyn std::error::Error + Send + Sync>>(
        &fixture.data,
        |source, stage| {
            let node_did = node_did.clone();
            let owner = owner.clone();
            let reader = reader.clone();
            async move {
                let src = open_encrypted_node(&source, key, &node_did).await?;
                let src_skills_resp = exec_as(
                    &src,
                    &owner,
                    "{ Skill { skill_id node_did name instructions enabled } }",
                )
                .await?;
                let src_skills =
                    gents_protocol::graphql::graphql_rows_from_response(&src_skills_resp, "Skill");
                assert_eq!(src_skills.len(), 1, "{src_skills_resp}");
                src.shutdown().await?;
                drop(src);

                let staged = Arc::new(
                    persistent_builder(&stage, key, DEFAULT_BLOCK_CACHE_BYTES)?
                        .with_node_identity_did(node_did.clone())
                        .with_node_acp_owner(owner.as_str())
                        .build()
                        .await?,
                );
                grant_node_runtime_nac(&staged, &owner, &node_did).await?;
                grant_read_nac(&staged, &owner, &reader).await?;
                let added_policy = staged
                    .add_dac_policy_with_counter_as(
                        &actor_context(&node_did),
                        BuiltInPolicy::Household.definition(),
                        0,
                    )
                    .await?;
                assert_eq!(added_policy, policy_id);

                let rebound_skill_sdl = ids.render(&schemas::SKILL.replacen(
                    "resource: \"configuration\"",
                    "resource: \"secret_configuration\"",
                    1,
                ));
                staged.add_collection(&rebound_skill_sdl).await?;

                let row = &src_skills[0];
                let _new_skill_doc = create_as(
                    &staged,
                    &owner,
                    "Skill",
                    &format!(
                        r#"mutation {{ create_Skill(input: {{ skill_id: "{}", node_did: "{}", name: "{}", instructions: "{}", enabled: {} }}) {{ _docID }} }}"#,
                        row["skill_id"].as_str().unwrap(),
                        row["node_did"].as_str().unwrap(),
                        row["name"].as_str().unwrap(),
                        row["instructions"].as_str().unwrap(),
                        row["enabled"].as_bool().unwrap()
                    ),
                )
                .await?;
                staged.shutdown().await?;
                drop(staged);

                let manifest = StoreManifest::for_migrated_store(
                    LINEAGE_VERSION,
                    target_lineage,
                    DocumentAcp::Local,
                    ManifestPolicyIds::from_home_policy_ids(ids),
                    "synthetic-v4-resource-rebind".to_owned(),
                );
                write_manifest(&stage, &manifest)?;
                write_provenance_marker_for_lineage(
                    &stage,
                    key,
                    target_lineage,
                    storage::backends::DurabilityMode::Immediate,
                )
                .await?;
                Ok(())
            }
        },
    )
    .await?;

    let promoted = open_encrypted_node(&fixture.data, &fixture.key, fixture.node.did()).await?;
    assert_eq!(
        promoted
            .get_collection(schemas::SKILL_NAME)?
            .unwrap()
            .policy
            .unwrap()
            .resource_name,
        "secret_configuration"
    );
    let owner_after = exec_as(
        &promoted,
        &fixture.owner,
        "{ Skill { skill_id instructions } }",
    )
    .await?;
    let owner_after_rows =
        gents_protocol::graphql::graphql_rows_from_response(&owner_after, "Skill");
    assert_eq!(owner_after_rows.len(), 1, "{owner_after}");
    assert_eq!(owner_after_rows[0]["instructions"], "keep-secret");
    let reader_after = exec_as(
        &promoted,
        &delegated_reader,
        "{ Skill { skill_id instructions } }",
    )
    .await?;
    assert_eq!(
        gents_protocol::graphql::graphql_rows_from_response(&reader_after, "Skill").len(),
        0,
        "delegated reader must no longer read Skill after resource migration"
    );
    promoted.shutdown().await?;

    finish_staged_migration(&fixture.data, true)?;
    Ok(())
}

#[tokio::test]
async fn staged_migration_preserves_source_on_failure_and_resumes_every_crash_boundary(
) -> TestResult {
    let fixture = create_fresh_1_0_home(61).await?;

    // Forbidden and boot-only change kinds are rejected before creating any journal.
    assert!(matches!(
        begin_staged_migration(
            &fixture.data,
            LINEAGE_VERSION,
            LINEAGE_VERSION + 1,
            ChangeKind::IdentityDerivationChange,
        ),
        Err(StoreMigrationError::ChangeNotStaged {
            kind: ChangeKind::IdentityDerivationChange
        })
    ));
    assert!(matches!(
        begin_staged_migration(
            &fixture.data,
            LINEAGE_VERSION,
            LINEAGE_VERSION + 1,
            ChangeKind::NewCollection,
        ),
        Err(StoreMigrationError::ChangeNotStaged {
            kind: ChangeKind::NewCollection
        })
    ));
    assert!(load_journal(&fixture.data)?.is_none());

    // A failure during staging leaves the source store byte-identical and blocks finish.
    let before = store_bytes(&fixture.data)?;
    begin_staged_migration(
        &fixture.data,
        LINEAGE_VERSION,
        LINEAGE_VERSION + 1,
        ChangeKind::AdditiveNullableField,
    )?;
    let failed =
        run_staged_migration::<_, _, StoreMigrationError>(&fixture.data, |_, stage| async move {
            fs::write(stage.join("partial"), b"incomplete")?;
            Err(StoreMigrationError::Io(std::io::Error::other(
                "simulated staging failure",
            )))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        store_bytes(&fixture.data)?,
        before,
        "failed staging leaves the original 1.0 store byte-identical"
    );
    assert!(matches!(
        finish_staged_migration(&fixture.data, true),
        Err(StoreMigrationError::CannotFinish { .. })
    ));

    // Resume from each crash boundary (0 = before retire, 1 = after retire, 2 = after promote).
    let ids = HomePolicyIds::node_store();
    for crash_after_renames in 0..=2 {
        let crash_home = create_fresh_1_0_home(70 + crash_after_renames as u8).await?;
        let target_lineage = LINEAGE_VERSION + 1;
        begin_staged_migration(
            &crash_home.data,
            LINEAGE_VERSION,
            target_lineage,
            ChangeKind::AdditiveNullableField,
        )?;
        let stage = staging_path(&crash_home.data);
        fs::create_dir(&stage)?;
        copy_store_tree(&crash_home.data, &stage)?;
        fs::remove_file(stage.join(PROVENANCE_MARKER))?;
        write_manifest(
            &stage,
            &StoreManifest::for_migrated_store(
                LINEAGE_VERSION,
                target_lineage,
                DocumentAcp::Local,
                ManifestPolicyIds::from_home_policy_ids(ids),
                "crash-resume-fingerprint".to_owned(),
            ),
        )?;
        write_provenance_marker_for_lineage(
            &stage,
            &crash_home.key,
            target_lineage,
            storage::backends::DurabilityMode::Immediate,
        )
        .await?;
        save_journal(
            &crash_home.data,
            &StagedMigrationJournal {
                version: MIGRATION_JOURNAL_VERSION,
                from_lineage: LINEAGE_VERSION,
                to_lineage: target_lineage,
                change_kind: ChangeKind::AdditiveNullableField,
                phase: MigrationPhase::Verified,
            },
            false,
        )?;
        if crash_after_renames >= 1 {
            fs::rename(&crash_home.data, retired_path(&crash_home.data))?;
        }
        if crash_after_renames >= 2 {
            fs::rename(&stage, &crash_home.data)?;
        }

        // Resuming from Verified never re-invokes staging and finishes cleanly.
        run_staged_migration::<_, _, StoreMigrationError>(&crash_home.data, |_, _| async {
            panic!("resuming after verification must not restage")
        })
        .await?;

        // Finish is refused while target_accepted is false, then succeeds once true.
        assert!(matches!(
            finish_staged_migration(&crash_home.data, false),
            Err(StoreMigrationError::CannotFinish { .. })
        ));
        assert!(retired_path(&crash_home.data).exists());
        finish_staged_migration(&crash_home.data, true)?;
        assert!(!retired_path(&crash_home.data).exists());
        assert!(load_journal(&crash_home.data)?.is_none());
        let manifest = read_manifest(&crash_home.data)?.expect("manifest after resume");
        assert_eq!(manifest.lineage, target_lineage);
        assert_eq!(manifest.migrated_from, Some(LINEAGE_VERSION));
    }

    Ok(())
}
