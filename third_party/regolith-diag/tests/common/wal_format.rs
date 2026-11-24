//! The format 2 write-ahead log layout, as the tests that edit log bytes
//! need it, and a listener that records the discards an open reports.
//!
//! Written from the format's documentation (`src/engine/wal_frame.rs`),
//! not from the engine's reader, so a test checks the engine against the
//! format rather than against itself:
//!
//! ```text
//! stamp   ["REGO"][format u16 = 2][reserved u16][head check u32][nonce u64][check u32]
//! record  [len u32][kind u8][synced_through u64][payload check u32][header check u32]
//!         [payload: len bytes]
//! ```

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use regolith::{EventListener, WalTailDiscardedInfo};

/// Bytes in a format 2 stamp.
pub const STAMP_LEN: usize = 24;
/// Bytes of framing in front of every record's payload.
pub const HEADER_LEN: usize = 21;
/// The kind byte of a commit group's record.
pub const KIND_GROUP: u8 = 0xC0;
/// The kind byte of the record a clean close appends last.
pub const KIND_CLOSE: u8 = 0xC1;

/// Whether `bytes` begin with a format 2 stamp.
pub fn is_format_2(bytes: &[u8]) -> bool {
    bytes.len() >= STAMP_LEN && &bytes[0..4] == b"REGO" && bytes[4..6] == 2u16.to_le_bytes()
}

/// Where every record of an undamaged format 2 log starts, then where the
/// last one ends. Trusts each record's length, so only for a log as the
/// engine wrote it.
pub fn record_bounds(bytes: &[u8]) -> Vec<usize> {
    assert!(is_format_2(bytes), "not a format 2 log");
    let mut bounds = vec![STAMP_LEN];
    loop {
        let at = *bounds.last().unwrap();
        if at == bytes.len() {
            return bounds;
        }
        assert!(
            at + HEADER_LEN <= bytes.len(),
            "a record header runs past the file"
        );
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        bounds.push(at + HEADER_LEN + len);
    }
}

/// The kind byte of the record at `at`.
pub fn kind_at(bytes: &[u8], at: usize) -> u8 {
    bytes[at + 4]
}

/// The `synced_through` of the record at `at`.
pub fn synced_through_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at + 5..at + 13].try_into().unwrap())
}

/// Records every [`WalTailDiscardedInfo`] an open reports.
#[derive(Default)]
pub struct TailReports(Mutex<Vec<WalTailDiscardedInfo>>);

impl TailReports {
    /// A recorder to register with `Options::listeners`.
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Every report so far.
    pub fn taken(&self) -> Vec<WalTailDiscardedInfo> {
        self.0.lock().unwrap().clone()
    }
}

impl EventListener for TailReports {
    fn on_wal_tail_discarded(&self, info: &WalTailDiscardedInfo) {
        self.0.lock().unwrap().push(info.clone());
    }
}

/// The header check of the record whose header starts with `fields` (its
/// first 17 bytes are read), at `offset` of the log stamped with `nonce`.
pub fn header_check(nonce: u64, offset: u64, fields: &[u8]) -> u32 {
    // "rego-hdr" in ASCII, the seed the format fixes.
    const HEADER_SEED: u64 = 0x7265_676F_2D68_6472;
    let mut input = Vec::with_capacity(33);
    input.extend_from_slice(&nonce.to_le_bytes());
    input.extend_from_slice(&offset.to_le_bytes());
    input.extend_from_slice(&fields[..17]);
    xxhash_rust::xxh3::xxh3_64_with_seed(&input, HEADER_SEED) as u32
}

/// The nonce in a format 2 log's stamp.
pub fn nonce_of(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[12..20].try_into().unwrap())
}

/// Split the format 2 log `bytes` at the record boundary `at` into two
/// logs, as a rotation there would have left them: the first `at` bytes,
/// and a log under the same stamp holding the records from `at` on. Their
/// headers are signed again for their new offsets, claiming nothing
/// synced, which is what a fresh log's records claim before its first
/// sync.
pub fn split_at(bytes: &[u8], at: usize) -> (Vec<u8>, Vec<u8>) {
    let nonce = nonce_of(bytes);
    let mut second = bytes[..STAMP_LEN].to_vec();
    let bounds = record_bounds(bytes);
    assert!(bounds.contains(&at), "{at} is not a record boundary");
    for w in bounds.windows(2).filter(|w| w[0] >= at) {
        let offset = second.len() as u64;
        let mut header = bytes[w[0]..w[0] + HEADER_LEN].to_vec();
        header[5..13].copy_from_slice(&0u64.to_le_bytes());
        let check = header_check(nonce, offset, &header);
        header[17..21].copy_from_slice(&check.to_le_bytes());
        second.extend_from_slice(&header);
        second.extend_from_slice(&bytes[w[0] + HEADER_LEN..w[1]]);
    }
    (bytes[..at].to_vec(), second)
}
