//! What encryption at rest costs.
//!
//! `encryption/block` is the per-block cost on the read and write paths,
//! with the same primitives and the same copies the engine makes:
//!
//! - `decode_checksum`: an unsealed block's read, a checksum over the frame
//!   and the copy of its payload into the block's own buffer;
//! - `decode_sealed`: a sealed block's read, the copy of its ciphertext into
//!   the block's own buffer and AES-256-GCM-SIV decryption in place;
//! - `encode_checksum` and `encode_sealed`: the matching write, a checksum
//!   against a fresh nonce from the operating system and encryption in place.
//!
//! `encryption/read` is the same cost in place: point reads and a forward
//! scan over tables with the block cache off, so every read decodes a block,
//! on an unencrypted and an encrypted database built alike. The difference
//! per read is what a block decode costs under encryption, framing included.

mod common;

use std::hint::black_box;
use std::sync::Arc;

use aes_gcm_siv::aead::{AeadInOut, KeyInit};
use aes_gcm_siv::{Aes256GcmSiv, Nonce, Tag};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use regolith::{Db, KeyId, KeyMaterial, KeyProvider, Options, WriteBatch, WriteOptions};
use xxhash_rust::xxh3::Xxh3Default;

const BLOCK_SIZES: [usize; 2] = [4096, 16 * 1024];
const N_KEYS: u64 = 100_000;
const VALUE_LEN: usize = 100;
const READS: u64 = 10_000;

/// The checksum an unsealed block carries: a domain-prefixed xxh3 over the
/// compression byte and the payload, as `engine::checksum::sst_block` is.
fn checksum(payload: &[u8]) -> u32 {
    let mut hasher = Xxh3Default::new();
    hasher.update(b"regolith/sst-block/v2");
    hasher.update(&1u64.to_le_bytes());
    hasher.update(&[0]);
    hasher.update(&(payload.len() as u64).to_le_bytes());
    hasher.update(payload);
    hasher.digest() as u32
}

fn aad() -> [u8; 26] {
    let mut aad = [7u8; 26];
    aad[0] = b'S';
    aad
}

fn block_costs(c: &mut Criterion) {
    let cipher = Aes256GcmSiv::new(&[0x42u8; 32].into());
    let mut group = c.benchmark_group("encryption/block");
    for size in BLOCK_SIZES {
        group.throughput(Throughput::Bytes(size as u64));
        let mut rng = common::Rng::new(size as u64);
        let plain = common::rand_value(&mut rng, size);
        let mut sealed = plain.clone();
        let tag = cipher
            .encrypt_inout_detached(
                &Nonce::from([9u8; 12]),
                &aad(),
                sealed.as_mut_slice().into(),
            )
            .unwrap();

        group.bench_function(BenchmarkId::new("decode_checksum", size), |b| {
            b.iter(|| {
                let sum = checksum(black_box(&plain));
                black_box(sum);
                black_box(plain.to_vec())
            })
        });
        group.bench_function(BenchmarkId::new("decode_sealed", size), |b| {
            b.iter(|| {
                let mut block = black_box(&sealed).to_vec();
                cipher
                    .decrypt_inout_detached(
                        &Nonce::from([9u8; 12]),
                        &aad(),
                        block.as_mut_slice().into(),
                        &Tag::from(tag),
                    )
                    .unwrap();
                block
            })
        });
        group.bench_function(BenchmarkId::new("encode_checksum", size), |b| {
            b.iter(|| checksum(black_box(&plain)))
        });
        group.bench_function(BenchmarkId::new("encode_sealed", size), |b| {
            let mut out = plain.clone();
            b.iter(|| {
                let mut nonce = [0u8; 12];
                getrandom::fill(&mut nonce).unwrap();
                cipher
                    .encrypt_inout_detached(&Nonce::from(nonce), &aad(), out.as_mut_slice().into())
                    .unwrap()
            })
        });
    }
    group.finish();
}

struct BenchKey;

impl KeyProvider for BenchKey {
    fn current(&self) -> KeyId {
        KeyId(1)
    }

    fn key(&self, _: KeyId) -> Option<KeyMaterial> {
        Some(KeyMaterial::new([0x42; 32]))
    }
}

/// Fill, flush and compact, so every key is in a table, then close and
/// reopen with the block cache off.
fn build(encrypted: bool) -> (common::TempDb, Db) {
    let options = || {
        let opts = Options::default()
            .write_buffer_size(4 * 1024 * 1024)
            .block_size(4096)
            .block_cache_size(0);
        if encrypted {
            opts.key_provider(Arc::new(BenchKey))
        } else {
            opts
        }
    };
    let tag = if encrypted { "enc-sealed" } else { "enc-plain" };
    let (tmp, db) = common::open(tag, options());
    let wopts = WriteOptions {
        disable_wal: true,
        ..WriteOptions::default()
    };
    let mut rng = common::Rng::new(0xE4C);
    for start in (0..N_KEYS).step_by(1000) {
        let mut batch = WriteBatch::new();
        for i in start..(start + 1000).min(N_KEYS) {
            batch.put(&common::key(i), &common::rand_value(&mut rng, VALUE_LEN));
        }
        db.write_opt(&wopts, batch).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    db.close().unwrap();
    drop(db);
    let db = Db::open(tmp.path(), options()).unwrap();
    (tmp, db)
}

fn read_costs(c: &mut Criterion) {
    let mut group = c.benchmark_group("encryption/read");
    for encrypted in [false, true] {
        let name = if encrypted { "sealed" } else { "plain" };
        let (_tmp, db) = build(encrypted);
        let mut rng = common::Rng::new(1);
        let keys: Vec<Vec<u8>> = (0..READS)
            .map(|_| common::key(rng.next() % N_KEYS))
            .collect();
        group.throughput(Throughput::Elements(READS));
        group.bench_function(BenchmarkId::new("point_uncached", name), |b| {
            b.iter(|| {
                for key in &keys {
                    black_box(db.get(key).unwrap());
                }
            })
        });
        group.throughput(Throughput::Elements(N_KEYS));
        group.bench_function(BenchmarkId::new("scan_uncached", name), |b| {
            b.iter(|| {
                let mut iter = db.iter();
                iter.seek_to_first();
                let mut n = 0u64;
                while iter.valid() {
                    n += 1;
                    iter.next();
                }
                iter.status().unwrap();
                assert_eq!(n, N_KEYS);
            })
        });
    }
    group.finish();
}

criterion_group!(benches, block_costs, read_costs);
criterion_main!(benches);
