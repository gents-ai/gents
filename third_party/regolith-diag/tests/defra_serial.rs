//! Property: at DefraLevel the committed transactions are the same as those
//! transactions run one at a time in commit order (the invariant of
//! `all_classes_serial` in `proofs/lean/Regolith/Validation.lean`).
//!
//! Random schedules interleave transactions that decide on a whole value, on
//! one part of a value, on a validated range, and on nothing, with blind
//! writes, flushes and compactions. A transaction decides what to write from
//! what it read at its snapshot. After the run each committed transaction is
//! replayed against the state the earlier ones left, deciding again from that
//! state: it must decide the same writes (the decision at its snapshot has the
//! effect of the decision at its commit point), and the database must end in
//! the state the replay reaches.
//!
//! A second property runs the same schedules on a database with a key
//! classifier, so the three classes it can declare are inside the guarantee
//! too, each used the way its contract allows:
//!
//! * content-addressed blocks, which a transaction stores when a read did not
//!   find them, stores again beside a reference it adds, and deletes when
//!   nothing refers to them;
//! * a commutative prefix, where a transaction scans the members and adds one
//!   without deciding on what the scan returned, beside a scan that leaves the
//!   prefix and writes back a document it was returned;
//! * a log, appended to by transactions that read its head and decide nothing
//!   on it, with and without a once key.
//!
//! A write-free transaction is placed at its snapshot, not at its commit, as the
//! proof does: it reads one point in time, and writes nothing that could move.
//!
//! A mutant that stops validating a rule (a part read that ignores operands, a
//! value read that compares nothing, a presence read that is not checked, a
//! delete of a block that is exempt) makes this fail, which is how the rules
//! were checked to be load-bearing.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use common::parted::{PARTS, Parted, add, parts_of, value};
use proptest::prelude::*;
use regolith::{
    Db, IsolationLevel, KeyClass, KeyClassifier, LogLayout, MemEnv, OptimisticTransactionDb,
    Options, ScanCheck, ScanDirection, Transaction, TransactionError, TxnOptions,
};

const KEYS: usize = 3;
/// The key a range decision writes its total to; outside the scanned range.
const TOTAL: usize = KEYS;
/// Blocks: content-addressed keys.
const BLOCKS: usize = 2;
/// The first of the documents that count the references to each block.
const REFS: usize = TOTAL + 1;
/// Every key that holds a value of three parts.
const DOCS: usize = REFS + BLOCKS;
/// Members a transaction can add to the commutative prefix.
const MEMBERS: usize = 3;
/// Once keys an append can name: one, so that appends meet on it.
const ONCE: usize = 1;
const SLOTS: usize = 3;

/// The keys `b/` are blocks named by their bytes, the keys `h/` a set whose
/// writers add members under their own names, and the journal a log.
struct Classes;

impl KeyClassifier for Classes {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else if key.starts_with(b"h/") {
            KeyClass::CommutativePrefix { len: 2 }
        } else if key.starts_with(b"log") || key.starts_with(b"once/") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

/// Entries at `log/<20 decimal digits>`, the head at `log-head`.
struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"log-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("log/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        "log/".len() + 20
    }
}

/// The first and the last key of the commutative prefix, there from the
/// start, so a scan of the prefix walks one stretch over every member.
const FIRST_MEMBER: &[u8] = b"h/a";
const LAST_MEMBER: &[u8] = b"h/z";

/// What the database holds, as far as the transactions here read and write
/// it. Also what one transaction saw of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    /// The documents, the total and the reference counts, by index.
    docs: [Option<[u64; PARTS]>; DOCS],
    /// Whether each block is stored. Its bytes follow from its key.
    blocks: [bool; BLOCKS],
    /// Whether each member is in the prefix.
    members: [bool; MEMBERS],
    /// The log's entries, in position order.
    log: Vec<u64>,
    /// The position each once key holds.
    once: [Option<u64>; ONCE],
}

/// The key of document `key`.
fn name(key: usize) -> Vec<u8> {
    match key {
        TOTAL => b"t0".to_vec(),
        refs if refs >= REFS => format!("r{}", refs - REFS).into_bytes(),
        _ => format!("k{key}").into_bytes(),
    }
}

fn block_key(block: usize) -> Vec<u8> {
    format!("b/{block}").into_bytes()
}

/// The bytes of block `block`: the key determines them.
fn block_bytes(block: usize) -> Vec<u8> {
    format!("the bytes of block {block}").into_bytes()
}

fn member_key(member: usize) -> Vec<u8> {
    format!("h/m{member}").into_bytes()
}

fn once_key(once: usize) -> Vec<u8> {
    format!("once/{once}").into_bytes()
}

fn entry_key(position: u64) -> Vec<u8> {
    format!("log/{position:020}").into_bytes()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Put(usize, [u64; PARTS]),
    Delete(usize),
    Add(usize, u32, u64),
    PutBlock(usize),
    DeleteBlock(usize),
    /// Add a member and rewrite the last key of the prefix with the bytes it
    /// always holds.
    Join(usize),
    /// Append an entry, under a once key or under none.
    Append(u64, Option<usize>),
}

/// What a transaction reads, and decides from.
#[derive(Clone, Debug)]
enum Prog {
    /// Read a value, write it back with the first part one higher.
    ReadModifyWrite(usize),
    /// Read a value and write it back unchanged, if it is there.
    Rewrite(usize),
    /// Read one part of `from`, add what it read plus one to a part of `to`.
    ProjectedAdd {
        from: usize,
        part: u32,
        to: usize,
        to_part: u32,
    },
    /// Scan every key with a validated range, add the sum of one part, plus
    /// one, to the total.
    RangeTotal(u32),
    /// Read some keys and write nothing.
    ReadOnly(Vec<usize>),
    /// Write without reading.
    Blind(Op),
    /// Read a block, store it when the read did not find it, and count the
    /// use in a part of a document.
    Store { block: usize, to: usize, part: u32 },
    /// Read a block, store it again whatever the read found, and add one to
    /// its count of references.
    Link(usize),
    /// Read a block, and its count of references by no parts; delete the
    /// block when it is there and the count is zero. The count is not
    /// validated: what keeps the decision current is that whoever adds a
    /// reference stores the block again, and a delete of a block is validated
    /// against that.
    Collect(usize),
    /// Scan the commutative prefix and add a member, deciding nothing on the
    /// scan.
    Join(usize),
    /// Scan from the commutative prefix through the documents with a plain
    /// scan, and write back a document it was returned with the first part
    /// one higher: the stretch leaves the prefix, so it is recorded.
    ScanRewrite(usize),
    /// Read the log's head and append an entry that owes nothing to it.
    Append(u64, Option<usize>),
}

impl Prog {
    /// Whether no commit of this program may be refused, whatever commits
    /// around it: every read it decides nothing on is exempt, and every
    /// write it makes is one its class never validates or one that leaves
    /// what any other writer of the key leaves.
    fn never_conflicts(&self) -> bool {
        matches!(self, Self::Link(_) | Self::Join(_) | Self::Append(..))
    }

    /// Whether two decisions of this program are the same when they have the
    /// same effect, though they differ as lists: a block it stores only when
    /// it is missing is stored by someone either way.
    fn decides_by_effect(&self) -> bool {
        matches!(self, Self::Store { .. })
    }
}

/// The writes `prog` decides on from `seen`, the values it read.
fn decide(prog: &Prog, seen: &State) -> Vec<Op> {
    match prog {
        Prog::ReadModifyWrite(key) => {
            let mut v = seen.docs[*key].unwrap_or([0; PARTS]);
            v[0] = v[0].wrapping_add(1);
            vec![Op::Put(*key, v)]
        }
        Prog::Rewrite(key) => seen.docs[*key]
            .map(|v| Op::Put(*key, v))
            .into_iter()
            .collect(),
        Prog::ProjectedAdd {
            from,
            part,
            to,
            to_part,
        } => {
            // Only the named part of `from` and whether it is there.
            let read = seen.docs[*from].map_or(0, |v| v[*part as usize]);
            vec![Op::Add(*to, *to_part, read.wrapping_add(1))]
        }
        Prog::RangeTotal(part) => {
            let total: u64 = (0..KEYS)
                .map(|key| seen.docs[key].map_or(0, |v| v[*part as usize]))
                .fold(0, u64::wrapping_add);
            vec![Op::Add(TOTAL, 0, total.wrapping_add(1))]
        }
        Prog::ReadOnly(_) => Vec::new(),
        Prog::Blind(op) => vec![op.clone()],
        Prog::Store { block, to, part } => {
            let store = (!seen.blocks[*block]).then_some(Op::PutBlock(*block));
            store.into_iter().chain([Op::Add(*to, *part, 1)]).collect()
        }
        Prog::Link(block) => vec![Op::PutBlock(*block), Op::Add(REFS + *block, 0, 1)],
        Prog::Collect(block) => {
            let referred = seen.docs[REFS + *block].map_or(0, |v| v[0]);
            let collect = seen.blocks[*block] && referred == 0;
            collect
                .then_some(Op::DeleteBlock(*block))
                .into_iter()
                .collect()
        }
        Prog::Join(member) => vec![Op::Join(*member)],
        Prog::ScanRewrite(key) => seen.docs[*key]
            .map(|mut v| {
                v[0] = v[0].wrapping_add(1);
                Op::Put(*key, v)
            })
            .into_iter()
            .collect(),
        Prog::Append(entry, once) => vec![Op::Append(*entry, *once)],
    }
}

fn decode(bytes: Option<impl AsRef<[u8]>>) -> Option<[u64; PARTS]> {
    bytes.map(|bytes| parts_of(bytes.as_ref()).expect("a value of three parts"))
}

/// Run the reads of `prog` in `tx`, and return what they saw.
fn observe(prog: &Prog, tx: &Transaction) -> State {
    let mut seen = State::default();
    match prog {
        Prog::ReadModifyWrite(key) | Prog::Rewrite(key) => {
            seen.docs[*key] = decode(tx.get(&name(*key)).unwrap());
        }
        Prog::ProjectedAdd { from, part, .. } => {
            seen.docs[*from] = decode(tx.get_parts(&name(*from), &[*part]).unwrap());
        }
        Prog::RangeTotal(_) => {
            let mut cursor = tx.cursor(
                Some(b"k0"),
                Some(b"k9"),
                ScanDirection::Forward,
                ScanCheck::Range,
            );
            loop {
                let page = cursor.next_page(tx, 64).unwrap();
                for (key, bytes) in page.entries {
                    let index = usize::from(key[1] - b'0');
                    seen.docs[index] = decode(Some(bytes));
                }
                if page.done {
                    break;
                }
            }
        }
        Prog::ReadOnly(keys) => {
            for key in keys {
                seen.docs[*key] = decode(tx.get(&name(*key)).unwrap());
            }
        }
        Prog::Blind(_) => {}
        Prog::Store { block, .. } | Prog::Link(block) => {
            seen.blocks[*block] = tx.get(&block_key(*block)).unwrap().is_some();
        }
        Prog::Collect(block) => {
            seen.blocks[*block] = tx.get(&block_key(*block)).unwrap().is_some();
            // By no parts: never validated.
            let referred = tx.get_parts(&name(REFS + *block), &[]).unwrap();
            seen.docs[REFS + *block] = decode(referred);
        }
        Prog::Join(_) => {
            // The whole prefix and nothing else: the stretch stays inside it.
            let members = tx.scan_stream(Some(b"h/"), Some(b"h0")).count();
            assert!(
                members >= 2,
                "the first and the last member are always there"
            );
        }
        Prog::ScanRewrite(key) => {
            // From the prefix through the documents: the stretch leaves it.
            for item in tx.scan_stream(Some(b"h/"), Some(b"k9")) {
                let (scanned, bytes) = item.unwrap();
                if scanned == name(*key) {
                    seen.docs[*key] = decode(Some(bytes));
                }
            }
        }
        Prog::Append(..) => {
            // A read of a log key: never validated, and nothing rests on it.
            tx.get(Journal.head_key()).unwrap();
        }
    }
    seen
}

fn buffer(tx: &Transaction, op: &Op) {
    match op {
        Op::Put(key, parts) => tx.put(&name(*key), &value(*parts)).unwrap(),
        Op::Delete(key) => tx.delete(&name(*key)).unwrap(),
        Op::Add(key, part, delta) => tx.merge(&name(*key), &add(*part, *delta)).unwrap(),
        Op::PutBlock(block) => tx.put(&block_key(*block), &block_bytes(*block)).unwrap(),
        Op::DeleteBlock(block) => tx.delete(&block_key(*block)).unwrap(),
        Op::Join(member) => {
            tx.put(&member_key(*member), b"member").unwrap();
            tx.put(LAST_MEMBER, b"member").unwrap();
        }
        Op::Append(entry, once) => {
            let log: Arc<dyn LogLayout> = Arc::new(Journal);
            let once = once.map(once_key);
            tx.append(&log, &entry.to_be_bytes(), once.as_deref())
                .unwrap();
        }
    }
}

/// How the engine applies a write to the state.
fn apply(state: &mut State, op: &Op) {
    match op {
        Op::Put(key, parts) => state.docs[*key] = Some(*parts),
        Op::Delete(key) => state.docs[*key] = None,
        Op::Add(key, part, delta) => {
            let mut parts = state.docs[*key].unwrap_or([0; PARTS]);
            parts[*part as usize] = parts[*part as usize].wrapping_add(*delta);
            state.docs[*key] = Some(parts);
        }
        Op::PutBlock(block) => state.blocks[*block] = true,
        Op::DeleteBlock(block) => state.blocks[*block] = false,
        Op::Join(member) => state.members[*member] = true,
        Op::Append(entry, once) => {
            // An entry under a once key that holds a position is not appended.
            if once.is_some_and(|once| state.once[once].is_some()) {
                return;
            }
            state.log.push(*entry);
            if let Some(once) = once {
                state.once[*once] = Some(state.log.len() as u64);
            }
        }
    }
}

/// The state after `ops`, applied to `state` in order.
fn after(state: &State, ops: &[Op]) -> State {
    let mut state = state.clone();
    ops.iter().for_each(|op| apply(&mut state, op));
    state
}

#[derive(Clone, Debug)]
enum Action {
    Begin(usize, Prog),
    Commit(usize),
    Flush,
    Compact,
}

fn key() -> impl Strategy<Value = usize> {
    0..KEYS
}

fn block() -> impl Strategy<Value = usize> {
    0..BLOCKS
}

/// The programs that touch no key of any class: what a transaction at
/// DefraLevel does where no classifier is installed.
fn plain_prog() -> impl Strategy<Value = Prog> {
    let part = 0..PARTS as u32;
    prop_oneof![
        3 => key().prop_map(Prog::ReadModifyWrite),
        2 => key().prop_map(Prog::Rewrite),
        4 => (key(), part.clone(), key(), part.clone()).prop_map(|(from, part, to, to_part)| {
            Prog::ProjectedAdd { from, part, to, to_part }
        }),
        2 => part.clone().prop_map(Prog::RangeTotal),
        1 => proptest::collection::vec(key(), 1..3).prop_map(Prog::ReadOnly),
        3 => (key(), part.clone(), 0u64..3).prop_map(|(k, p, d)| Prog::Blind(Op::Add(k, p, d))),
        1 => (key(), proptest::array::uniform3(0u64..3))
            .prop_map(|(k, parts)| Prog::Blind(Op::Put(k, parts))),
        1 => key().prop_map(|k| Prog::Blind(Op::Delete(k))),
    ]
}

/// The programs on keys of the three classes, which need the classifier.
fn classed_prog() -> impl Strategy<Value = Prog> {
    let part = 0..PARTS as u32;
    prop_oneof![
        4 => (block(), key(), part).prop_map(|(block, to, part)| Prog::Store { block, to, part }),
        4 => block().prop_map(Prog::Link),
        4 => block().prop_map(Prog::Collect),
        2 => block().prop_map(|b| Prog::Blind(Op::PutBlock(b))),
        2 => block().prop_map(|b| Prog::Blind(Op::DeleteBlock(b))),
        3 => (0..MEMBERS).prop_map(Prog::Join),
        3 => key().prop_map(Prog::ScanRewrite),
        // Two in three under a once key, so that one is used again.
        5 => (0u64..1000, prop_oneof![1 => Just(None), 2 => (0..ONCE).prop_map(Some)])
            .prop_map(|(entry, once)| Prog::Append(entry, once)),
    ]
}

fn action(prog: impl Strategy<Value = Prog>) -> impl Strategy<Value = Action> {
    prop_oneof![
        6 => (0..SLOTS, prog).prop_map(|(slot, prog)| Action::Begin(slot, prog)),
        6 => (0..SLOTS).prop_map(Action::Commit),
        1 => Just(Action::Flush),
        1 => Just(Action::Compact),
    ]
}

fn position(db: &Db, key: &[u8]) -> Option<u64> {
    db.get(key)
        .unwrap()
        .map(|bytes| u64::from_be_bytes(bytes.as_slice().try_into().expect("an 8-byte position")))
}

fn read_state(db: &Db) -> State {
    let mut state = State::default();
    for (key, slot) in state.docs.iter_mut().enumerate() {
        *slot = decode(db.get(&name(key)).unwrap());
    }
    for (block, stored) in state.blocks.iter_mut().enumerate() {
        let bytes = db.get(&block_key(block)).unwrap();
        assert!(
            bytes
                .as_ref()
                .is_none_or(|bytes| *bytes == block_bytes(block)),
            "block {block} holds other bytes than its key names"
        );
        *stored = bytes.is_some();
    }
    for (member, joined) in state.members.iter_mut().enumerate() {
        *joined = db.get(&member_key(member)).unwrap().is_some();
    }
    for p in 1..=position(db, Journal.head_key()).unwrap_or(0) {
        let entry = db
            .get(&entry_key(p))
            .unwrap()
            .unwrap_or_else(|| panic!("position {p} is missing below the head"));
        state
            .log
            .push(u64::from_be_bytes(entry.as_slice().try_into().unwrap()));
    }
    for (once, held) in state.once.iter_mut().enumerate() {
        *held = position(db, &once_key(once));
    }
    state
}

/// What a transaction reads of `state`, as its program reads it.
fn seen_in(prog: &Prog, state: &State) -> State {
    let mut seen = State::default();
    match prog {
        Prog::ReadModifyWrite(k) | Prog::Rewrite(k) | Prog::ScanRewrite(k) => {
            seen.docs[*k] = state.docs[*k];
        }
        Prog::ProjectedAdd { from, .. } => seen.docs[*from] = state.docs[*from],
        Prog::RangeTotal(_) => seen.docs[..KEYS].copy_from_slice(&state.docs[..KEYS]),
        Prog::ReadOnly(keys) => keys.iter().for_each(|k| seen.docs[*k] = state.docs[*k]),
        Prog::Store { block, .. } | Prog::Link(block) => seen.blocks[*block] = state.blocks[*block],
        Prog::Collect(block) => {
            seen.blocks[*block] = state.blocks[*block];
            seen.docs[REFS + *block] = state.docs[REFS + *block];
        }
        Prog::Blind(_) | Prog::Join(_) | Prog::Append(..) => {}
    }
    seen
}

/// Run `actions` against a database seeded with `seeded` documents and
/// `stored` blocks, with the classifier installed when `classified`, and
/// check the committed transactions against their replay in commit order.
fn run_schedule(
    classified: bool,
    seeded: [Option<[u64; PARTS]>; 4],
    stored: [bool; BLOCKS],
    actions: &[Action],
) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let options = Options::default().merge_operator(Some(Arc::new(Parted)));
    let db = if classified {
        // In memory: the classes are rules of the commit, and a schedule of
        // flushes and compactions exercises them the same without a disk.
        let options = options
            .env(Arc::new(MemEnv::new()))
            .max_background_compactions(0);
        OptimisticTransactionDb::open(dir.path(), options)
            .unwrap()
            .with_policy(Arc::new(Classes))
    } else {
        OptimisticTransactionDb::open(dir.path(), options).unwrap()
    };
    for (key, parts) in seeded.iter().enumerate() {
        if let Some(parts) = parts {
            db.db().put(&name(key), &value(*parts)).unwrap();
        }
    }
    for block in (0..BLOCKS).filter(|block| stored[*block]) {
        db.db().put(&block_key(block), &block_bytes(block)).unwrap();
    }
    for member in [FIRST_MEMBER, LAST_MEMBER] {
        db.db().put(member, b"member").unwrap();
    }
    let initial = read_state(db.db());

    // Prog, its writes, its transaction, and the writers committed when it began.
    type Slot = Option<(Prog, Vec<Op>, Transaction, usize)>;
    let mut open: Vec<Slot> = (0..SLOTS).map(|_| None).collect();
    let mut writers: Vec<(Prog, Vec<Op>)> = Vec::new();
    let mut write_free: Vec<(Prog, usize)> = Vec::new();
    let mut finish = |slot: &mut Slot, writers: &mut Vec<(Prog, Vec<Op>)>| {
        let Some((prog, ops, tx, snapshot)) = slot.take() else {
            return Ok(());
        };
        let free = ops.is_empty();
        match tx.commit() {
            Ok(_) if free => write_free.push((prog, snapshot)),
            Ok(_) => writers.push((prog, ops)),
            Err(TransactionError::Conflict(_)) => {
                prop_assert!(!free, "a write-free transaction never conflicts");
                prop_assert!(!prog.never_conflicts(), "{:?} was refused", prog);
            }
            Err(other) => prop_assert!(false, "unexpected {other:?}"),
        }
        Ok(())
    };

    for action in actions {
        match action {
            Action::Begin(slot, prog) => {
                if open[*slot].is_some() {
                    continue;
                }
                let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                let seen = observe(prog, &tx);
                let ops = decide(prog, &seen);
                ops.iter().for_each(|op| buffer(&tx, op));
                open[*slot] = Some((prog.clone(), ops, tx, writers.len()));
            }
            Action::Commit(slot) => finish(&mut open[*slot], &mut writers)?,
            Action::Flush => db.db().flush().unwrap(),
            Action::Compact => db.db().compact_range(None, None).wait().unwrap(),
        }
    }
    for slot in &mut open {
        finish(slot, &mut writers)?;
    }

    // The replay: each writer decides again from the state the ones before
    // it left, and a write-free transaction from the state its snapshot held.
    let mut state = initial;
    let mut states = vec![state.clone()];
    for (prog, ops) in &writers {
        let again = decide(prog, &seen_in(prog, &state));
        if prog.decides_by_effect() {
            prop_assert_eq!(
                after(&state, &again),
                after(&state, ops),
                "{:?} decided to another effect at its commit point",
                prog
            );
        } else {
            prop_assert_eq!(
                &again,
                ops,
                "{:?} decided differently at its commit point",
                prog
            );
        }
        // What it wrote is what was applied, whichever way it compared.
        state = after(&state, ops);
        states.push(state.clone());
    }
    for (prog, snapshot) in &write_free {
        let again = decide(prog, &seen_in(prog, &states[*snapshot]));
        prop_assert!(again.is_empty(), "{:?} wrote something", prog);
    }
    prop_assert_eq!(
        read_state(db.db()),
        state,
        "the database is not the serial state"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// With no classifier: whole values, parts, validated ranges and
    /// write-free transactions.
    #[test]
    fn committed_transactions_equal_the_same_transactions_run_in_commit_order(
        seeded in proptest::array::uniform4(proptest::option::of(proptest::array::uniform3(0u64..3))),
        actions in proptest::collection::vec(action(plain_prog()), 1..30),
    ) {
        run_schedule(false, seeded, [false; BLOCKS], &actions)?;
    }

    /// With the classifier: the same programs beside the ones on
    /// content-addressed blocks, the commutative prefix and the log, three of
    /// those for each of the others.
    #[test]
    fn classified_transactions_equal_the_same_transactions_run_in_commit_order(
        seeded in proptest::array::uniform4(proptest::option::of(proptest::array::uniform3(0u64..3))),
        stored in proptest::array::uniform2(any::<bool>()),
        actions in proptest::collection::vec(
            action(prop_oneof![1 => plain_prog(), 3 => classed_prog()]),
            1..40,
        ),
    ) {
        run_schedule(true, seeded, stored, &actions)?;
    }
}
