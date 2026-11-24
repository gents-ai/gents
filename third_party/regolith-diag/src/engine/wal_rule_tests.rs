//! Format 2 replay: the rule `proofs/tla/WalRecovery.tla` checks, case by
//! case and against the model as an oracle.

use super::*;

use crate::engine::wal_frame::KIND_CLOSE;

/// A log of `n` put groups, each synced before the next is appended when
/// `sync_each`, and its bytes and record offsets.
fn log_of(dir: &TempDir, n: u64, sync_each: bool) -> (PathBuf, Vec<u8>, Vec<u64>) {
    let (mut wal, path) = new_wal(dir);
    for i in 0..n {
        wal.append_put(format!("k{i}").as_bytes(), b"value", i + 1)
            .unwrap();
        if sync_each {
            wal.sync_data().unwrap();
        }
    }
    drop(wal);
    let bytes = fs::read(&path).unwrap();
    let offsets = record_offsets(&bytes);
    (path, bytes, offsets)
}

/// RED `DropBelowP` (Lean `drop_below_p_loses_proven`). Group 0 was synced,
/// and group 1, written after that sync, says so. Damage to group 0 is
/// then below P: dropping it would lose a write a surviving record proves
/// durable, so the open is refused, naming the file, O and P.
#[test]
fn red_drop_below_p_damage_a_later_record_proves_synced_refuses() {
    let dir = TempDir::new().unwrap();
    let (path, _, offsets) = log_of(&dir, 2, true);
    flip_byte(&path, offsets[0] as usize + HEADER_LEN + 3);

    let err = replay_at(&path, WalPosition::Newest).expect_err("damage below P must refuse");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains("test.wal"), "{message}");
    assert!(message.contains(&offsets[0].to_string()), "O: {message}");
    assert!(message.contains(&offsets[1].to_string()), "P: {message}");
}

/// RED `RefuseAboveP` (Lean `refuse_above_p_never_opens`). Nothing was
/// synced; a crash kept group 0 and tore group 1. That crash honours
/// every flush, so the log opens with group 0, and the tail is reported.
#[test]
fn red_refuse_above_p_a_torn_record_nothing_proves_ends_the_log() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, offsets) = log_of(&dir, 2, false);
    fs::write(&path, &bytes[..bytes.len() - 4]).unwrap();

    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert_eq!(keys(&entries), vec![b"k0".to_vec()]);
    assert_eq!(
        tail,
        Some(TailVerdict {
            offset: offsets[1],
            discarded_bytes: bytes.len() as u64 - 4 - offsets[1],
        })
    );
}

/// RED `Format1` (Lean `format1_refuses_torn_tail`). Group 1 is
/// whole, its length on disk, but its bytes are wrong, and non-zero bytes
/// follow it: the state OPFS, FAT and ext4 writeback leave when a length
/// reaches the device before its data. Format 1 refuses it; format 2
/// opens with group 0 and reports the tail.
#[test]
fn red_format1_a_whole_bad_final_record_followed_by_garbage_is_a_tail() {
    let dir = TempDir::new().unwrap();
    let (path, mut bytes, offsets) = log_of(&dir, 2, false);
    for b in &mut bytes[offsets[1] as usize + HEADER_LEN..] {
        *b ^= 0x5A;
    }
    bytes.extend_from_slice(&[0x77; 4096]);
    fs::write(&path, &bytes).unwrap();

    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert_eq!(keys(&entries), vec![b"k0".to_vec()]);
    assert_eq!(
        tail,
        Some(TailVerdict {
            offset: offsets[1],
            discarded_bytes: bytes.len() as u64 - offsets[1],
        })
    );
}

/// The same crash with the tail zeroed, and with the whole unsynced region
/// garbled header included: every unusable record is alike.
#[test]
fn zeroed_and_garbled_tails_above_p_end_the_log() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, offsets) = log_of(&dir, 4, false);
    for fill in [0x00u8, 0xC0, 0xFF] {
        let mut damaged = bytes.clone();
        for b in &mut damaged[offsets[2] as usize..] {
            *b = fill;
        }
        fs::write(&path, &damaged).unwrap();
        let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
        assert_eq!(
            keys(&entries),
            vec![b"k0".to_vec(), b"k1".to_vec()],
            "{fill:#x}"
        );
        assert_eq!(tail.unwrap().offset, offsets[2], "{fill:#x}");
    }
}

/// RED `CloseWithoutSync`. A clean close syncs every record before it
/// appends CLOSE, so CLOSE claims the whole log before it, and syncs again
/// after it. Appended before the first sync, CLOSE could survive a crash
/// that lost a record before it, and replay would refuse that crash.
#[test]
fn red_close_without_sync_close_follows_a_sync_of_everything_before_it() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("closed.wal");
    let recording = Arc::new(crate::engine::recovery_tests::RecordingEnv::default());
    let env: Arc<dyn Env> = recording.clone();
    let mut wal = Wal::create_in(&env, &path, None).unwrap();
    wal.append_put(b"a", b"v", 1).unwrap();
    wal.append_put(b"b", b"v", 2).unwrap();
    let before_close = wal.offset();
    *recording.events.lock().unwrap() = Default::default();
    wal.close().unwrap();
    let events = recording.events.lock().unwrap();
    assert_eq!(
        events.log,
        vec![
            "sync".to_string(),
            "write 21".to_string(),
            "sync".to_string()
        ],
        "sync, CLOSE, sync"
    );
    drop(events);

    let bytes = fs::read(&path).unwrap();
    let close = wal_frame::decode_header(
        &bytes[before_close as usize..],
        wal.nonce,
        before_close,
        false,
    )
    .expect("CLOSE ends the log");
    assert_eq!(close.kind, KIND_CLOSE);
    assert_eq!(
        close.synced_through, before_close,
        "CLOSE claims everything before it"
    );
    assert_eq!(bytes.len() as u64, before_close + HEADER_LEN as u64);

    // Nothing follows CLOSE.
    assert!(wal.append_put(b"c", b"v", 3).is_err());
    wal.close().unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        keys(&Wal::replay(&path).unwrap()),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
}

/// The counterexample `CloseWithoutSync` guards against, built by hand: a
/// CLOSE that survived while a record before it did not. A usable CLOSE
/// proves the whole file durable, so replay refuses: which is right for
/// the file, and is why CLOSE must never be written before that sync.
#[test]
fn a_usable_close_proves_the_whole_file_so_damage_before_it_refuses() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"a", b"v", 1).unwrap();
    wal.append_put(b"b", b"v", 2).unwrap();
    wal.close().unwrap();
    drop(wal);
    let offsets = record_offsets(&fs::read(&path).unwrap());
    flip_byte(&path, offsets[1] as usize + HEADER_LEN + 2);
    let err = replay_at(&path, WalPosition::Newest).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn bytes_after_close_refuse() {
    let dir = TempDir::new().unwrap();
    let (mut wal, path) = new_wal(&dir);
    wal.append_put(b"a", b"v", 1).unwrap();
    wal.close().unwrap();
    drop(wal);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(&[0u8; 64]);
    fs::write(&path, &bytes).unwrap();
    assert!(replay_at(&path, WalPosition::Newest).is_err());
}

/// The promise `Residual` (Lean `residual_is_reported`): damage inside the
/// last synced group, which no later record vouches for, reads as a tail.
/// The open keeps the groups before it and reports the drop.
#[test]
fn damage_in_the_last_synced_group_is_a_reported_tail() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, offsets) = log_of(&dir, 3, true);
    flip_byte(&path, offsets[2] as usize + HEADER_LEN + 1);
    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert_eq!(keys(&entries), vec![b"k0".to_vec(), b"k1".to_vec()]);
    assert_eq!(
        tail,
        Some(TailVerdict {
            offset: offsets[2],
            discarded_bytes: bytes.len() as u64 - offsets[2],
        })
    );
}

/// P is read from records after the first unusable one, past a damaged
/// header whose length cannot be trusted: the scan finds the proving
/// record however many bytes and records the damage hides it behind.
#[test]
fn p_is_read_past_a_damaged_header_whose_length_lies() {
    let dir = TempDir::new().unwrap();
    let (path, _, offsets) = log_of(&dir, 6, true);
    // Make group 1's length claim the rest of the file and more.
    let mut bytes = fs::read(&path).unwrap();
    bytes[offsets[1] as usize..offsets[1] as usize + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    let err = replay_at(&path, WalPosition::Newest).unwrap_err();
    assert!(err.to_string().contains(&offsets[1].to_string()), "{err}");
}

/// A whole record copied from another log, or from elsewhere in this one,
/// into the unsynced tail never counts as proof: its header check is bound
/// to its own log and offset.
#[test]
fn records_of_another_log_or_another_offset_prove_nothing() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, offsets) = log_of(&dir, 3, true);
    let other_path = dir.path().join("other.wal");
    let other = {
        let mut wal = Wal::create(&other_path).unwrap();
        for i in 0..3u64 {
            wal.append_put(format!("o{i}").as_bytes(), b"value", i + 1)
                .unwrap();
            wal.sync_data().unwrap();
        }
        drop(wal);
        fs::read(&other_path).unwrap()
    };
    let mut damaged = bytes[..offsets[2] as usize].to_vec();
    // Group 2 is replaced by the other log's group 2, at the same offset,
    // and then group 1 of this log follows it again.
    damaged.extend_from_slice(&other[offsets[2] as usize..]);
    damaged.extend_from_slice(&bytes[offsets[1] as usize..offsets[2] as usize]);
    fs::write(&path, &damaged).unwrap();

    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert_eq!(keys(&entries), vec![b"k0".to_vec(), b"k1".to_vec()]);
    assert_eq!(tail.unwrap().offset, offsets[2]);
}

/// A header that verifies but claims more bytes than the file holds sizes
/// no buffer: the file ends inside that record, which is a torn tail.
#[test]
fn a_verified_header_running_past_the_file_allocates_nothing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("long.wal");
    let mut bytes = wal_frame::encode_stamp(3).to_vec();
    let claimed = vec![0u8; 1 << 20];
    bytes.extend_from_slice(&wal_frame::encode_header(
        KIND_GROUP,
        &claimed,
        0,
        3,
        STAMP_LEN as u64,
    ));
    fs::write(&path, &bytes).unwrap();
    let mut iter =
        WalReplayIter::open(&crate::env::std_env(), &path, WalPosition::Newest, None).unwrap();
    assert!(iter.next_entry().unwrap().is_none());
    assert_eq!(iter.high_water_bytes(), 0);
    assert_eq!(iter.discarded_tail().unwrap().offset, STAMP_LEN as u64);
}

/// A group whose checks pass but whose operations do not parse is
/// unusable whole: none of its operations is yielded.
#[test]
fn a_group_whose_operations_do_not_parse_yields_none_of_them() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("malformed.wal");
    let mut payload = put_entries(b"first", 1);
    payload.push(0x09);
    let mut bytes = wal_frame::encode_stamp(3).to_vec();
    bytes.extend_from_slice(&wal_frame::encode_header(
        KIND_GROUP,
        &payload,
        0,
        3,
        STAMP_LEN as u64,
    ));
    bytes.extend_from_slice(&payload);
    fs::write(&path, &bytes).unwrap();
    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert!(entries.is_empty());
    assert_eq!(tail.unwrap().offset, STAMP_LEN as u64);
}

/// An earlier log was synced whole when it was sealed, so any unusable
/// record in it is damage, wherever it is and whatever proves it.
#[test]
fn any_unusable_record_in_an_earlier_log_refuses() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, offsets) = log_of(&dir, 3, false);
    for cut in [bytes.len() - 1, offsets[2] as usize + 3] {
        fs::write(&path, &bytes[..cut]).unwrap();
        let err = replay_at(&path, WalPosition::Earlier).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "cut at {cut}");
        assert!(err.to_string().contains("newer log"), "{err}");
    }
    fs::write(&path, &bytes).unwrap();
    let (entries, tail) = replay_at(&path, WalPosition::Earlier).unwrap();
    assert_eq!(entries.len(), 3);
    assert!(tail.is_none());
}

/// A crash at any byte keeps exactly the whole groups before the cut, for
/// a log nothing was synced in, and for one synced after every group.
#[test]
fn a_cut_at_every_offset_keeps_exactly_the_whole_groups_before_it() {
    let dir = TempDir::new().unwrap();
    for sync_each in [false, true] {
        let (path, bytes, offsets) = log_of(&dir, 4, sync_each);
        for cut in STAMP_LEN..=bytes.len() {
            fs::write(&path, &bytes[..cut]).unwrap();
            let whole = offsets.iter().filter(|&&o| o <= cut as u64).count() - 1;
            let (entries, tail) = replay_at(&path, WalPosition::Newest)
                .unwrap_or_else(|e| panic!("a cut at {cut} is a torn tail: {e}"));
            assert_eq!(entries.len(), whole, "cut at {cut}");
            assert_eq!(
                tail.is_some(),
                !offsets.contains(&(cut as u64)),
                "cut at {cut}"
            );
        }
    }
}

/// Every single-byte flip of a synced log either refuses or keeps a
/// prefix of the groups and reports the rest, never keeping a group after
/// a dropped one (`NoGap`), and never dropping a group a surviving record
/// proves synced (`NoProvenLoss`).
#[test]
fn every_byte_flip_refuses_or_keeps_a_reported_prefix() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, offsets) = log_of(&dir, 4, true);
    for at in STAMP_LEN..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x01;
        fs::write(&path, &damaged).unwrap();
        let damaged_group = offsets.iter().filter(|&&o| o <= at as u64).count() - 1;
        match replay_at(&path, WalPosition::Newest) {
            Ok((entries, tail)) => {
                assert_eq!(entries.len(), damaged_group, "flip at {at}");
                assert_eq!(tail.unwrap().offset, offsets[damaged_group], "flip at {at}");
                assert_eq!(damaged_group, 3, "only the last group has no later proof");
            }
            Err(_) => assert!(damaged_group < 3, "flip at {at} refused"),
        }
    }
}

/// A crash while a log is created leaves zeros or garbage where its stamp
/// should be, and nothing in the log was synced. Rot in a written stamp's
/// magic leaves the rest of the stamp, or the records after it, still
/// verifying: that log held synced writes, so it is refused, never
/// discarded as one a crash caught at birth.
#[test]
fn a_stamp_whose_magic_rotted_is_damage_not_a_log_cut_at_birth() {
    let dir = TempDir::new().unwrap();
    let (path, bytes, _) = log_of(&dir, 3, true);
    for byte in 0..4 {
        let mut rotted = bytes.clone();
        rotted[byte] ^= 0x01;
        fs::write(&path, &rotted).unwrap();
        let err = replay_at(&path, WalPosition::Newest).unwrap_err();
        assert!(err.to_string().contains("header is damaged"), "{err}");
    }
    // The stamp alone, with its magic rotted: its own checks still pass.
    let mut stamp_only = bytes[..STAMP_LEN].to_vec();
    stamp_only[1] ^= 0x40;
    fs::write(&path, &stamp_only).unwrap();
    assert!(replay_at(&path, WalPosition::Newest).is_err());
    // A format 1 stamp likewise.
    let mut v1 = crate::engine::wal_v1::stamp().to_vec();
    v1.extend_from_slice(&crate::engine::wal_v1::put(b"k", b"v", 1));
    v1[0] ^= 0x01;
    fs::write(&path, &v1).unwrap();
    assert!(replay_at(&path, WalPosition::Newest).is_err());

    // What a crash at birth leaves: no stamp check passes and no record
    // verifies, so the log is empty and every byte of it is reported.
    let mut garbled = bytes.clone();
    for b in &mut garbled[..STAMP_LEN] {
        *b = 0x33;
    }
    fs::write(&path, &garbled).unwrap();
    let (entries, tail) = replay_at(&path, WalPosition::Newest).unwrap();
    assert!(entries.is_empty());
    assert_eq!(tail.unwrap().discarded_bytes, garbled.len() as u64);
}

/// What a crash leaves of one record past the synced prefix
/// (`WalRecovery.tla`, `UnsyncedKinds`, with a partial record as one more
/// unusable shape).
#[derive(Clone, Copy, Debug)]
enum Fate {
    Intact,
    Garbage,
    Zero,
    /// The file ends inside it; only for the last record kept.
    Partial,
}

/// A run of the writer and a crash state of its newest log, as
/// `WalRecovery.tla` draws them.
#[derive(Clone, Debug)]
struct CrashCase {
    /// One group per entry, synced after it when `true`.
    syncs: Vec<bool>,
    close: bool,
    /// Records the file keeps, at least the synced prefix.
    keep: usize,
    fates: Vec<Fate>,
    /// Damage the last synced record (`Residual`).
    rot: bool,
    seed: u64,
    /// The log is sealed (`Frame = "AEAD"`).
    sealed: bool,
    /// A sealed log is opened under the wrong key (`keyOk = FALSE`).
    wrong_key: bool,
}

fn crash_case() -> impl proptest::strategy::Strategy<Value = CrashCase> {
    use proptest::prelude::*;
    let fate = prop_oneof![
        3 => Just(Fate::Intact),
        1 => Just(Fate::Garbage),
        1 => Just(Fate::Zero),
        1 => Just(Fate::Partial),
    ];
    (
        proptest::collection::vec(any::<bool>(), 1..9),
        any::<bool>(),
        0usize..11,
        proptest::collection::vec(fate, 10),
        any::<bool>(),
        any::<u64>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(syncs, close, keep, fates, rot, seed, sealed, wrong_key)| CrashCase {
                syncs,
                close,
                keep,
                fates,
                rot,
                seed,
                sealed,
                wrong_key,
            },
        )
}

/// Seeded bytes, never a valid record but by astronomical chance.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// The verdict `WalRecovery.tla` gives a crash state: `None` when
/// recovery refuses, else how many leading records it keeps and whether
/// it reports a discarded tail. `usable[i]` says whether the file's
/// record `i` is usable, `claims[i]` is its stamp in records, as the
/// model's stamps count, and `close_at` is CLOSE's position.
fn model_verdict(
    usable: &[bool],
    claims: &[usize],
    close_at: Option<usize>,
) -> Option<(usize, bool)> {
    let o = usable.iter().position(|u| !u).unwrap_or(usable.len());
    let p = (0..usable.len())
        .filter(|&i| usable[i])
        .map(|i| {
            if Some(i) == close_at {
                i + 1
            } else {
                claims[i]
            }
        })
        .max()
        .unwrap_or(0);
    // "O < P" in bytes is, with 0-based record positions, o < p.
    if o < usable.len() && o < p {
        return None;
    }
    Some((o, o < usable.len()))
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

    /// The code's replay gives every crash state the verdict the model
    /// gives it, and so keeps the model's invariants: it never refuses a
    /// crash the flush honoured (`RecoveryOpens`), keeps every synced
    /// record (`KeepsSynced`), keeps a prefix (`NoGap`), never drops a
    /// record a surviving stamp proves synced (`NoProvenLoss`), and
    /// reports any loss of synced data it opens on (`LossIsReported`).
    ///
    /// Half the cases seal the log (`MC_WalRecovery_Green_Aead`): a failed
    /// tag is an unusable record under the same rule, and an open under the
    /// wrong key refuses (`WrongKeyRefuses`) instead of dropping the log.
    #[test]
    fn replay_gives_every_crash_state_the_models_verdict(case in crash_case()) {
        let dir = TempDir::new().unwrap();
        let ring = crate::engine::seal::test_keys::keyring(&[1]);
        let path = dir.path().join("test.wal");
        let mut wal = Wal::create_in(
            &crate::env::std_env(),
            &path,
            case.sealed.then_some(&*ring),
        )
        .unwrap();
        let mut claims = Vec::new();
        let mut synced = 0usize;
        for (i, sync) in case.syncs.iter().enumerate() {
            claims.push(synced);
            wal.append_put(format!("k{i}").as_bytes(), b"value", i as u64 + 1).unwrap();
            if *sync {
                wal.sync_data().unwrap();
                synced = i + 1;
            }
        }
        let groups = case.syncs.len();
        let mut close_at = None;
        if case.close {
            claims.push(synced);
            close_at = Some(groups);
            wal.close().unwrap();
            synced = groups + 1;
        }
        drop(wal);
        let pristine = fs::read(&path).unwrap();
        let offsets = record_offsets(&pristine);
        let records = offsets.len() - 1;

        // The crash: the synced prefix survives, but for the rotted
        // record; the file keeps `kept` records, each past the prefix
        // given its fate, a partial one only last.
        let kept = case.keep.clamp(synced, records);
        let mut bytes = pristine[..stamp_len_of(&pristine)].to_vec();
        let mut usable = Vec::new();
        for i in 0..kept {
            let record = &pristine[offsets[i] as usize..offsets[i + 1] as usize];
            let fate = if i < synced {
                if case.rot && i + 1 == synced {
                    Fate::Garbage
                } else {
                    Fate::Intact
                }
            } else {
                match case.fates[i] {
                    Fate::Partial if i + 1 < kept => Fate::Garbage,
                    fate => fate,
                }
            };
            match fate {
                Fate::Intact => bytes.extend_from_slice(record),
                Fate::Garbage => {
                    bytes.extend_from_slice(&noise(case.seed ^ i as u64, record.len()))
                }
                Fate::Zero => bytes.extend(std::iter::repeat_n(0u8, record.len())),
                Fate::Partial => {
                    let cut = 1 + (case.seed as usize % (record.len() - 1));
                    bytes.extend_from_slice(&record[..cut]);
                }
            }
            usable.push(matches!(fate, Fate::Intact));
        }
        fs::write(&path, &bytes).unwrap();

        let wrong_key = case.sealed && case.wrong_key;
        let verdict = if wrong_key {
            None
        } else {
            model_verdict(&usable, &claims[..kept], close_at.filter(|&c| c < kept))
        };
        let rotted = case.rot && synced > 0;
        let opened_with = if wrong_key {
            crate::engine::seal::test_keys::wrong_keyring(&[1])
        } else {
            ring
        };
        match (replay_with(&path, WalPosition::Newest, Some(&opened_with)), verdict) {
            (Err(e), None) if wrong_key => {
                proptest::prop_assert!(
                    e.to_string().contains("does not verify under key id 1"),
                    "a wrong key refused for another reason: {}",
                    e
                );
            }
            (Err(e), None) => {
                proptest::prop_assert!(rotted, "refused a crash the flush honoured: {}", e);
            }
            (Ok((entries, tail)), Some((keep_records, discarded))) => {
                let kept_groups = keep_records.min(groups);
                proptest::prop_assert_eq!(entries.len(), kept_groups);
                for (i, entry) in entries.iter().enumerate() {
                    proptest::prop_assert_eq!(seq(entry), i as u64 + 1, "a gap");
                }
                proptest::prop_assert_eq!(tail.is_some(), discarded);
                if let Some(tail) = tail {
                    proptest::prop_assert_eq!(tail.offset, offsets[keep_records]);
                    proptest::prop_assert_eq!(
                        tail.discarded_bytes,
                        bytes.len() as u64 - offsets[keep_records]
                    );
                }
                let synced_groups = synced.min(groups);
                if rotted {
                    proptest::prop_assert!(
                        kept_groups >= synced_groups || discarded,
                        "lost synced data without reporting it"
                    );
                } else {
                    proptest::prop_assert!(kept_groups >= synced_groups, "lost a synced group");
                }
            }
            (got, want) => proptest::prop_assert!(
                false,
                "code gave {:?}, the model gives {:?}",
                got.map(|(e, t)| (e.len(), t)),
                want
            ),
        }
    }
}
