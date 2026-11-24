use std::path::PathBuf;

use super::super::*;
use super::*;
use crate::engine::seal::test_keys::{TestKeys, keyring};
use crate::env::MemEnv;

fn fixture() -> (Arc<dyn Env>, PathBuf, PathBuf) {
    let env: Arc<dyn Env> = Arc::new(MemEnv::new());
    let db_dir = PathBuf::from("sealed-manifest");
    let sst_dir = db_dir.join("sst");
    env.create_dir_all(&sst_dir).unwrap();
    (env, db_dir, sst_dir)
}

fn open(
    env: &Arc<dyn Env>,
    db_dir: &Path,
    sst_dir: &Path,
    ring: Option<Arc<Keyring>>,
) -> io::Result<VersionSet> {
    VersionSet::open_with_policy(env, db_dir, sst_dir, MetadataPolicy::Pinned, ring)
}

fn manifest(env: &Arc<dyn Env>, db_dir: &Path) -> Vec<u8> {
    env.read(&db_dir.join("MANIFEST")).unwrap()
}

/// Offsets where each batch's `len` field starts, after the stamp.
fn batch_offsets(data: &[u8], stamp: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = stamp;
    while at + 4 <= data.len() {
        out.push(at);
        let len = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        at += 4 + len + 4;
    }
    out
}

#[test]
fn a_sealed_manifest_round_trips_and_names_no_edit_in_plaintext() {
    let (env, db_dir, sst_dir) = fixture();
    let ring = keyring(&[4]);
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[
        VersionEdit::SetLastSeq(0x1122_3344_5566),
        VersionEdit::SetNextFileId(0x77_8899),
    ])
    .unwrap();
    drop(vs);

    let data = manifest(&env, &db_dir);
    assert_eq!(&data[..7], b"REGOMAN");
    assert_eq!(data[7], MANIFEST_FORMAT_SEALED);
    let edit = 0x1122_3344_5566u64.to_le_bytes();
    assert!(
        !data.windows(6).any(|w| w == &edit[..6]),
        "an edit is in the manifest in plaintext"
    );
    let vs = open(&env, &db_dir, &sst_dir, Some(ring)).unwrap();
    assert_eq!(vs.current().last_seq, 0x1122_3344_5566);
    assert_eq!(vs.current().next_file_id, 0x77_8899);
}

#[test]
fn a_sealed_manifest_without_a_keyring_refuses() {
    let (env, db_dir, sst_dir) = fixture();
    drop(open(&env, &db_dir, &sst_dir, Some(keyring(&[1]))).unwrap());
    for result in [
        open(&env, &db_dir, &sst_dir, None).map(|_| ()),
        VersionSet::open_read_only(&env, &db_dir, &sst_dir, MetadataPolicy::Pinned, None)
            .map(|_| ()),
    ] {
        assert!(matches!(
            crate::Error::from(result.err().unwrap()),
            crate::Error::KeyProviderRequired
        ));
    }
}

#[test]
fn an_unsealed_manifest_is_sealed_by_the_first_open_with_a_keyring() {
    let (env, db_dir, sst_dir) = fixture();
    let mut vs = open(&env, &db_dir, &sst_dir, None).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(41)]).unwrap();
    drop(vs);
    assert_eq!(manifest(&env, &db_dir)[7], MANIFEST_FORMAT_V1);

    let ring = keyring(&[1]);
    let ro = VersionSet::open_read_only(
        &env,
        &db_dir,
        &sst_dir,
        MetadataPolicy::Pinned,
        Some(ring.clone()),
    )
    .unwrap();
    assert_eq!(ro.current().last_seq, 41);
    assert_eq!(
        manifest(&env, &db_dir)[7],
        MANIFEST_FORMAT_V1,
        "a read-only open writes nothing"
    );
    drop(ro);

    let vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    assert_eq!(vs.current().last_seq, 41);
    drop(vs);
    assert_eq!(manifest(&env, &db_dir)[7], MANIFEST_FORMAT_SEALED);
    assert_eq!(
        open(&env, &db_dir, &sst_dir, Some(ring))
            .unwrap()
            .current()
            .last_seq,
        41
    );
}

#[test]
fn batches_name_their_key_across_a_rotation_and_a_rewrite_retires_the_old_one() {
    let (env, db_dir, sst_dir) = fixture();
    let keys = TestKeys::new(&[1, 2]);
    let ring = Arc::new(Keyring::new(keys.clone()));
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(10)]).unwrap();
    keys.set_current(2);
    vs.apply(&[VersionEdit::SetNextFileId(20)]).unwrap();
    drop(vs);

    let vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    assert_eq!((vs.current().last_seq, vs.current().next_file_id), (10, 20));
    drop(vs);

    let only_two = keyring(&[2]);
    let err = open(&env, &db_dir, &sst_dir, Some(only_two.clone()))
        .err()
        .unwrap();
    assert!(matches!(
        crate::Error::from(err),
        crate::Error::UnknownKey { id: KeyId(1) }
    ));

    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring)).unwrap();
    vs.compact_manifest().unwrap();
    drop(vs);
    let vs = open(&env, &db_dir, &sst_dir, Some(only_two)).unwrap();
    assert_eq!((vs.current().last_seq, vs.current().next_file_id), (10, 20));
}

#[test]
fn a_torn_sealed_batch_is_dropped_like_any_torn_batch() {
    let (env, db_dir, sst_dir) = fixture();
    let ring = keyring(&[1]);
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(5)]).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(6)]).unwrap();
    drop(vs);
    let data = manifest(&env, &db_dir);
    for cut in 1..20 {
        let mut torn = data.clone();
        torn.truncate(data.len() - cut);
        env.write(&db_dir.join("MANIFEST"), &torn).unwrap();
        let vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
        assert_eq!(vs.current().last_seq, 5, "cut {cut}");
    }
}

/// The batch a crash tore with its length intact: zeros or garbage where
/// its body was. Its checksum fails before any key is consulted, so it is
/// dropped like a format 1 torn batch, and the open keeps the batches
/// before it. Were the tag to replace the checksum here, the torn batch
/// could not be told from one under a wrong key: refusing it would refuse a
/// crash the open must survive, and dropping it would drop a wrong key's
/// whole manifest.
#[test]
fn a_torn_batch_with_its_length_intact_is_dropped_before_any_key_is_used() {
    let (env, db_dir, sst_dir) = fixture();
    let ring = keyring(&[1]);
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(5)]).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(6)]).unwrap();
    drop(vs);
    let data = manifest(&env, &db_dir);
    let last = *batch_offsets(&data, SEALED_STAMP_LEN).last().unwrap();
    for fill in [0x00u8, 0x5A] {
        let mut torn = data.clone();
        for b in &mut torn[last + 4..] {
            *b = fill;
        }
        env.write(&db_dir.join("MANIFEST"), &torn).unwrap();
        let vs = open(&env, &db_dir, &sst_dir, Some(ring.clone()))
            .unwrap_or_else(|e| panic!("fill {fill:#x}: a torn batch refused the open: {e}"));
        assert_eq!(vs.current().last_seq, 5, "fill {fill:#x}");
    }
}

#[test]
fn a_batch_whose_checksum_holds_and_whose_tag_fails_refuses() {
    let (env, db_dir, sst_dir) = fixture();
    let ring = keyring(&[1]);
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(5)]).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(6)]).unwrap();
    drop(vs);
    let data = manifest(&env, &db_dir);
    let batches = batch_offsets(&data, SEALED_STAMP_LEN);
    let at = batches[0];
    let len = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    let body = at + 4..at + 4 + len;
    // Every byte of the sealed body, with the checksum recomputed so only
    // the tag can catch it: what tampering, not a torn write, looks like.
    for flip in body.clone() {
        let mut bad = data.clone();
        bad[flip] ^= 0x01;
        let sum = checksum::manifest_record(len as u32, &bad[body.clone()]);
        bad[body.end..body.end + 4].copy_from_slice(&sum.to_le_bytes());
        env.write(&db_dir.join("MANIFEST"), &bad).unwrap();
        let err = open(&env, &db_dir, &sst_dir, Some(ring.clone()))
            .err()
            .unwrap_or_else(|| panic!("a flip at byte {flip} opened"));
        if flip < body.start + 4 {
            // The key id leads the body: flipped, it names a key the
            // provider does not have, which is refused as such.
            assert!(
                matches!(crate::Error::from(err), crate::Error::UnknownKey { .. }),
                "flip at {flip}"
            );
        } else {
            assert!(
                err.to_string().contains(&format!(
                    "edit batch at offset {at} does not verify under key id 1"
                )),
                "flip at {flip}: {err}"
            );
        }
    }
    // A batch moved to another offset fails too: swap the two batches.
    let (a, b) = (batches[0], batches[1]);
    let mut swapped = data[..a].to_vec();
    swapped.extend_from_slice(&data[b..]);
    swapped.extend_from_slice(&data[a..b]);
    env.write(&db_dir.join("MANIFEST"), &swapped).unwrap();
    let err = open(&env, &db_dir, &sst_dir, Some(ring)).err().unwrap();
    assert!(err.to_string().contains("does not verify"), "{err}");
}

#[test]
fn a_manifest_sealed_in_another_database_does_not_open_here() {
    let (env, db_dir, sst_dir) = fixture();
    let ring = keyring(&[1]);
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(5)]).unwrap();
    drop(vs);
    let first = manifest(&env, &db_dir);
    env.remove_file(&db_dir.join("MANIFEST")).unwrap();
    let mut vs = open(&env, &db_dir, &sst_dir, Some(ring.clone())).unwrap();
    vs.apply(&[VersionEdit::SetLastSeq(9)]).unwrap();
    drop(vs);
    // The second manifest's stamp, the first one's batch: different salts.
    let mut spliced = manifest(&env, &db_dir)[..SEALED_STAMP_LEN].to_vec();
    spliced.extend_from_slice(&first[SEALED_STAMP_LEN..]);
    env.write(&db_dir.join("MANIFEST"), &spliced).unwrap();
    let err = open(&env, &db_dir, &sst_dir, Some(ring)).err().unwrap();
    assert!(err.to_string().contains("does not verify"), "{err}");
}
