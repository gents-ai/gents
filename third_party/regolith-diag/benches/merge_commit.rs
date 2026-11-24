use regolith::{DurabilityMode, MergeOperator, OptimisticTransactionDb, Options, TxnOptions};
use std::{
    sync::{Arc, Barrier},
    time::Instant,
};

struct Append;
impl MergeOperator for Append {
    fn name(&self) -> &'static str {
        "framed-append"
    }
    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.unwrap_or_default().to_vec();
        for operand in operands {
            out.extend_from_slice(operand.strip_prefix(b"operand:")?);
        }
        Some(out)
    }
    fn partial_merge(&self, _: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let mut out = left.to_vec();
        out.extend_from_slice(right.strip_prefix(b"operand:")?);
        Some(out)
    }
}

fn main() {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    let writers: usize = args
        .first()
        .map(String::as_str)
        .unwrap_or("1")
        .parse()
        .unwrap();
    let shape = args.get(1).map(String::as_str).unwrap_or("unique");
    let durability = args.get(2).map(String::as_str).unwrap_or("eventual");
    assert!(matches!(durability, "immediate" | "eventual"));
    assert!(writers > 0);
    let durable = durability == "immediate";
    let rounds = 4;
    let count = match shape {
        "unique" => 512,
        "growing" => 10_000,
        _ => panic!("invalid shape"),
    };
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .durability(if durable {
                DurabilityMode::Immediate
            } else {
                DurabilityMode::Eventual
            })
            .write_buffer_size(256 * 1024 * 1024)
            .merge_operator(Some(Arc::new(Append))),
    )
    .unwrap();
    let start = Instant::now();
    let barrier = Barrier::new(writers);
    let mut latencies = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..writers)
            .map(|writer| {
                let db = &db;
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut samples = Vec::new();
                    barrier.wait();
                    for round in 0..rounds {
                        let tx = db.begin(&TxnOptions::new());
                        for index in 0..count {
                            let mut key = (writer as u64).to_be_bytes().to_vec();
                            key.extend_from_slice(
                                &(if shape == "unique" { index } else { round } as u64)
                                    .to_be_bytes(),
                            );
                            let mut operand = b"operand:".to_vec();
                            operand.extend_from_slice(&(index as u32).to_be_bytes());
                            tx.merge(&key, &operand).unwrap();
                        }
                        let start = Instant::now();
                        tx.commit().unwrap();
                        samples.push(start.elapsed().as_nanos() as u64);
                    }
                    samples
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    let seconds = start.elapsed().as_secs_f64();
    for writer in 0..writers {
        for slot in 0..if shape == "unique" { count } else { rounds } {
            let mut key = (writer as u64).to_be_bytes().to_vec();
            key.extend_from_slice(&(slot as u64).to_be_bytes());
            let expected = if shape == "unique" {
                (slot as u32).to_be_bytes().repeat(rounds)
            } else {
                (0..count as u32).flat_map(u32::to_be_bytes).collect()
            };
            assert_eq!(db.db().get(&key).unwrap().unwrap(), expected);
        }
    }
    latencies.sort_unstable();
    let transactions = writers * rounds;
    let rate = transactions as f64 / seconds;
    println!(
        "{{\"writers\":{writers},\"shape\":\"{shape}\",\"durability\":\"{durability}\",\"transactions\":{transactions},\"operands_per_transaction\":{count},\"seconds\":{seconds},\"transactions_per_second\":{rate},\"commit_ns\":{latencies:?}}}"
    );
}
