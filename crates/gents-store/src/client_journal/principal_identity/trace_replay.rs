use super::*;
use crate::test_support::blocking_steps::BlockingSteps;
use gents_lean_vocab::tla::replay::{Red, Trace};
use rapidhash::RapidHashMap;

async fn metadata(paths: &DesktopPaths) -> Result<Option<NodeIdentityMetadata>> {
    let path = paths.principal_metadata_path().to_owned();
    tokio::task::spawn_blocking(move || read_metadata(&path)).await?
}

async fn replay(trace: &Trace) {
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("desktop"));
    paths.ensure_root_dirs().await.unwrap();
    let mut steps = RapidHashMap::default();
    let mut tasks = Vec::new();
    let mut installed = false;
    let mut never_uninstalled = true;
    let mut complete = true;
    for step in &trace.steps {
        let who = step.actor.as_ref().unwrap()["$model"].as_str().unwrap();
        if step.action == "Read" {
            let (checkpoints, mut observe) = BlockingSteps::channel();
            assert!(steps.insert(who, checkpoints).is_none());
            let paths = paths.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                let mut awaiting_read = false;
                let identity = PrincipalIdentity::load_or_create_at(&paths, |phase| {
                    match phase {
                        MetadataStep::Rename => awaiting_read = true,
                        MetadataStep::Decide => awaiting_read = false,
                        _ => {}
                    }
                    observe(phase);
                });
                (identity, !awaiting_read)
            }));
        }
        let phase = match step.action.as_str() {
            "Read" => Some(MetadataStep::Read),
            "Create" => Some(MetadataStep::Create),
            "Fill" => Some(MetadataStep::Fill),
            "Rename" => Some(MetadataStep::Rename),
            "Decide" => Some(MetadataStep::Decide),
            "Clear" => None,
            "Crash" => {
                assert!(
                    !steps.contains_key(who),
                    "sampled process exited before opening metadata"
                );
                None
            }
            action => panic!("unmapped metadata action {action}"),
        };
        if let Some(phase) = phase {
            assert_eq!(steps.get_mut(who).unwrap().next().await, phase);
            installed |= phase == MetadataStep::Rename;
        }
        match metadata(&paths).await {
            Ok(None) => never_uninstalled &= !installed,
            Ok(Some(_)) => {}
            Err(_) => complete = false,
        }
    }
    drop(steps);
    let mut identities = Vec::new();
    let mut all_reread = true;
    for task in tasks {
        let (result, reread) = task.await.unwrap();
        all_reread &= reread;
        identities.push(result);
    }
    let all_accepted = identities.iter().all(Result::is_ok);
    let persisted = metadata(&paths).await.unwrap().unwrap();
    let agreed = identities.iter().flatten().all(|identity| {
        identity.did() == persisted.did && identity.public_key_bytes == persisted.public_key_bytes
    });
    trace.check(
        Red::Defect,
        &[
            ("NoMissingFileError", all_accepted),
            ("NoTornMetadata", complete),
            ("NeverUninstalled", never_uninstalled),
            ("AgreedInstall", agreed),
            ("OkSawOwnMetadata", agreed && all_reread),
        ],
    );
}

#[tokio::test]
async fn client_identity_metadata_traces() {
    for json in [
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadata.sample-1.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataBefore.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataDivergentKey.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataNoReread.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataRemoveThenRename.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataRemoveThenRenameReread.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataSharedStaging.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCClientIdentityMetadataSharedStagingTorn.json"),
    ] {
        replay(&Trace::load(json, "ClientIdentityMetadata", &[])).await;
    }
}
