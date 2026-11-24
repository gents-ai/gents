use super::*;
use crate::engine::seal::test_keys::{keyring, wrong_keyring};

/// The bytes no sealed backup may hold in plaintext: a table's key range.
const SMALLEST: &[u8] = b"smallest-user-key-in-the-table";
const LARGEST: &[u8] = b"largest-user-key-in-the-table";

fn entry(file_id: u64, seal_key: Option<KeyId>) -> BackupFileEntry {
    BackupFileEntry {
        level: 6,
        file_id,
        file_size: 100 + file_id,
        hash: 0xABCD_0000 + u128::from(file_id),
        smallest_key: SMALLEST.to_vec(),
        largest_key: LARGEST.to_vec(),
        num_entries: 2,
        global_seq: Some(5),
        seal_key,
    }
}

fn manifest(sealed_under: Option<KeyId>, seal_key: Option<KeyId>) -> BackupManifest {
    BackupManifest {
        created_at_unix: 1_700_000_000,
        files: vec![entry(9, seal_key), entry(12, None)],
        next_file_id: 13,
        last_seq: 5,
        sealed_under,
        range_deletes_checked: true,
    }
}

/// `m` sealed under key `key` of a keyring holding `ids`, as backup `id`.
fn sealed(m: &BackupManifest, id: u64, ids: &[u32], key: u32) -> Vec<u8> {
    let sealer = keyring(ids).sealer(KeyId(key)).unwrap();
    encode(m, BackupId(id), Some(&sealer)).unwrap()
}

/// Put back a valid checksum after a deliberate change, as someone who
/// meant the change would.
fn rechecksum(data: &mut [u8]) {
    let (body, sum) = data.split_at_mut(data.len() - 8);
    sum.copy_from_slice(&checksum::backup_manifest(body).to_le_bytes());
}

fn holds(data: &[u8], needle: &[u8]) -> bool {
    data.windows(needle.len()).any(|w| w == needle)
}

fn refusal(result: io::Result<BackupManifest>) -> crate::Error {
    crate::Error::from(result.expect_err("the metadata was accepted"))
}

#[test]
fn a_version_2_backup_manifest_still_reads() {
    let mut m = manifest(None, None);
    m.files.truncate(1);
    let current = decode(&encode(&m, BackupId(1), None).unwrap(), BackupId(1), None).unwrap();
    assert_eq!(current.files[0].global_seq, Some(5));

    // Version 2: the same body with no sequence per table.
    let mut body = encode(&m, BackupId(1), None).unwrap();
    body.truncate(body.len() - 8 - 8);
    let at = MAGIC.len();
    body[at..at + 4].copy_from_slice(&2u32.to_le_bytes());
    let checksum = checksum::backup_manifest(&body);
    body.extend_from_slice(&checksum.to_le_bytes());
    let older = decode(&body, BackupId(1), None).unwrap();
    assert_eq!(older.files[0].global_seq, None);
    assert_eq!(older.files[0].largest_key, LARGEST);
    // 0.1.x wrote version 2: a database restored from it is checked for
    // range deletes behind older keys at its first open; this build's own
    // backups were checked when their database opened.
    assert!(!older.range_deletes_checked);
    assert!(current.range_deletes_checked);
}

#[test]
fn a_plaintext_backup_stays_version_3_and_needs_no_key() {
    let m = manifest(None, None);
    let data = encode(&m, BackupId(3), None).unwrap();
    assert_eq!(&data[MAGIC.len()..HEAD_LEN], &VERSION_PLAIN.to_le_bytes());
    assert!(holds(&data, SMALLEST));
    assert_eq!(decode(&data, BackupId(3), None).unwrap(), m);
    // A keyring changes nothing for a backup that was never sealed.
    assert_eq!(decode(&data, BackupId(3), Some(&keyring(&[1]))).unwrap(), m);
    let listing = decode_listing(&data).unwrap();
    assert_eq!(listing.created_at_unix, m.created_at_unix);
    assert_eq!(
        listing.objects,
        vec![(m.files[0].hash, 109), (m.files[1].hash, 112)]
    );
}

#[test]
fn a_sealed_backup_round_trips_and_holds_no_key_range_in_plaintext() {
    let m = manifest(Some(KeyId(4)), Some(KeyId(2)));
    let data = sealed(&m, 7, &[2, 4], 4);
    assert_eq!(&data[..MAGIC.len()], &MAGIC);
    assert_eq!(&data[MAGIC.len()..HEAD_LEN], &VERSION_SEALED.to_le_bytes());
    for needle in [SMALLEST, LARGEST, &13u64.to_le_bytes()[..]] {
        assert!(!holds(&data, needle), "{needle:?} is in plaintext");
    }
    let ring = keyring(&[2, 4]);
    assert_eq!(decode(&data, BackupId(7), Some(&ring)).unwrap(), m);

    // The listing reads with no key, and says what the sealed body says.
    let listing = decode_listing(&data).unwrap();
    assert_eq!(listing.created_at_unix, m.created_at_unix);
    let expected: Vec<_> = m.files.iter().map(|f| (f.hash, f.file_size)).collect();
    assert_eq!(listing.objects, expected);
}

#[test]
fn a_sealed_backup_refuses_without_a_key_with_a_missing_key_and_with_a_wrong_key() {
    let data = sealed(&manifest(Some(KeyId(4)), None), 7, &[4], 4);
    assert!(matches!(
        refusal(decode(&data, BackupId(7), None)),
        crate::Error::KeyProviderRequired
    ));
    assert!(matches!(
        refusal(decode(&data, BackupId(7), Some(&keyring(&[1, 2])))),
        crate::Error::UnknownKey { id: KeyId(4) }
    ));
    let err = refusal(decode(&data, BackupId(7), Some(&wrong_keyring(&[4]))));
    assert!(matches!(err, crate::Error::Corruption(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("backup 7: its metadata does not verify under key id 4"),
        "{err}"
    );
}

#[test]
fn every_changed_byte_of_a_sealed_backup_refuses_with_or_without_its_checksum_fixed() {
    let m = manifest(Some(KeyId(1)), Some(KeyId(1)));
    let data = sealed(&m, 2, &[1, 2], 1);
    let ring = keyring(&[1, 2]);
    for i in 0..data.len() - 8 {
        let mut bad = data.clone();
        bad[i] ^= 0x01;
        // Accidental damage: the checksum, checked before any key.
        let err = decode(&bad, BackupId(2), Some(&ring)).unwrap_err();
        assert!(
            err.to_string().contains("checksum mismatch"),
            "byte {i}: {err}"
        );
        // A deliberate change keeps the checksum valid: the tag refuses it,
        // a listing change included, since the listing is associated data.
        rechecksum(&mut bad);
        match decode(&bad, BackupId(2), Some(&ring)) {
            Ok(back) => panic!("byte {i}: a changed backup decoded as {back:?}"),
            Err(e) => assert!(
                matches!(
                    crate::Error::from(e),
                    crate::Error::Corruption(_) | crate::Error::UnknownKey { .. }
                ),
                "byte {i}"
            ),
        }
    }
}

#[test]
fn a_listing_that_names_other_objects_refuses_the_restore() {
    let data = sealed(&manifest(Some(KeyId(1)), None), 2, &[1], 1);
    let mut bad = data.clone();
    // The first listed object's content id, swapped for another backup's.
    let at = HEAD_LEN + 8 + 4;
    bad[at..at + 16].copy_from_slice(&0xFEED_u128.to_le_bytes());
    rechecksum(&mut bad);
    assert_eq!(decode_listing(&bad).unwrap().objects[0].0, 0xFEED);
    let err = refusal(decode(&bad, BackupId(2), Some(&keyring(&[1]))));
    assert!(err.to_string().contains("does not verify"), "{err}");
}

#[test]
fn a_sealed_backup_read_under_another_backups_id_refuses() {
    let data = sealed(&manifest(Some(KeyId(1)), None), 2, &[1], 1);
    let ring = keyring(&[1]);
    assert!(decode(&data, BackupId(2), Some(&ring)).is_ok());
    let err = refusal(decode(&data, BackupId(3), Some(&ring)));
    assert!(err.to_string().contains("backup 3"), "{err}");
}

#[test]
fn the_metadata_names_its_key_so_it_opens_after_a_rotation() {
    let ring = keyring(&[1, 2]);
    let before = manifest(Some(KeyId(1)), Some(KeyId(1)));
    let data = encode(&before, BackupId(1), Some(&ring.sealer(KeyId(1)).unwrap())).unwrap();
    // The provider's current key is 2 now; the backup still names 1.
    let rotated =
        crate::engine::seal::Keyring::new(crate::engine::seal::test_keys::TestKeys::new(&[2, 1]));
    assert_eq!(rotated.current_id(), KeyId(2));
    let back = decode(&data, BackupId(1), Some(&rotated)).unwrap();
    assert_eq!(back.sealed_under, Some(KeyId(1)));
    assert_eq!(back, before);
}

#[test]
fn a_newer_version_is_refused() {
    let mut data = encode(&manifest(None, None), BackupId(1), None).unwrap();
    data[MAGIC.len()..HEAD_LEN].copy_from_slice(&5u32.to_le_bytes());
    rechecksum(&mut data);
    let err = decode_listing(&data).unwrap_err();
    assert!(
        err.to_string()
            .contains("unsupported backup manifest version 5"),
        "{err}"
    );
}

#[test]
fn a_hostile_table_count_is_refused_without_reserving_room_for_it() {
    for sealer in [None, Some(keyring(&[1]).current().unwrap())] {
        let mut data = encode(&manifest(None, None), BackupId(1), sealer.as_ref()).unwrap();
        let count_at = match sealer {
            None => HEAD_LEN + 8 * 3,
            Some(_) => HEAD_LEN + 8,
        };
        data[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        rechecksum(&mut data);
        assert!(decode_listing(&data).is_err());
        assert!(decode(&data, BackupId(1), Some(&keyring(&[1]))).is_err());
    }
}

#[test]
fn a_sealed_file_too_short_to_hold_its_seal_has_no_listing() {
    let data = sealed(&manifest(Some(KeyId(1)), None), 1, &[1], 1);
    let listing_end = HEAD_LEN + 8 + 4 + 2 * LISTING_ENTRY_LEN;
    let mut cut = data[..listing_end + 4].to_vec();
    cut.extend_from_slice(&[0; 8]);
    rechecksum(&mut cut);
    let err = decode_listing(&cut).unwrap_err();
    assert!(err.to_string().contains("ends before its seal"), "{err}");
}
