//! Assemble one schema v2 run file from every metric family the benches wrote.
//!
//! Usage:
//!   cargo bench --bench collect -- --out <run.json> --commit <sha> --label <label>
//!
//! Families are read from `$REGOLITH_BENCH_OUT` (JSON Lines) when it is set, and
//! from `./bench-out/*.json` otherwise. Criterion's own `estimates.json` files
//! are folded in as supporting per-iteration timings.
//!
//! Two rules the assembler exists to enforce. A family that was not collected
//! is written `trust: "absent"` and never as a zero, so a gap in collection
//! renders as a gap. And a run file is append-only once published: an existing
//! `--out` is an error, never an overwrite.

mod common;

#[path = "common/json.rs"]
mod json;
#[path = "common/records.rs"]
mod records;
#[path = "common/run_meta.rs"]
mod run_meta;
#[path = "common/timings.rs"]
mod timings;

use std::path::PathBuf;

use json::Json;
use records::Bag;
use run_meta::die;

/// Bench family name (with the aliases accepted for it) -> throughput slot.
const WORKLOADS: &[(&str, &[&str])] = &[
    ("read", &["point_read", "read"]),
    ("buffered_write", &["write_buffered", "buffered_write"]),
    ("durable_write", &["write_durable", "durable_write"]),
    ("scan", &["scan"]),
    ("batch", &["batch"]),
    ("large_value", &["large_value"]),
];
const SOAK: &[&str] = &["soak"];
const MEMORY: &[&str] = &["memory", "rss"];
const BINARY_SIZE: &[&str] = &["size", "binary_size"];
const CORRECTNESS: &[&str] = &["transaction", "correctness"];
const ISOLATION: &[&str] = &["isolation"];
const VIABILITY: &[&str] = &["viability"];
const RETRACTION: &[&str] = &["retraction"];
/// Per-operation allocation budgets. Deterministic: a count, not a rate,
/// so a busy runner cannot move it.
const ALLOCS: &[&str] = &["allocs"];
/// Peak live bytes a streaming scan holds against a materializing one.
/// Deterministic for the same reason as `ALLOCS`: bytes held, not a rate.
const SCAN_STREAM: &[&str] = &["scan_stream"];

/// Sub-metrics of the rss family that the memory bench contributes.
const RSS_PARTS: &[&str] = &["shard_sweep", "block_sweep", "point"];

const TRUST_LEVELS: [&str; 3] = ["clean", "contaminated", "absent"];

fn main() {
    let args = match parse_args() {
        Some(a) => a,
        None => return,
    };

    let files = records::record_files();
    let mut bag = records::read_records(&files);
    if bag.is_empty() {
        eprintln!(
            "collect: no family records found in {}. Every family will be written trust: \
             \"absent\".",
            records::describe(&files)
        );
    }

    let guard = run_meta::loadguard(&args.out);
    let throughput_trust = if guard.passed {
        "clean"
    } else {
        "contaminated"
    };

    let correctness = deterministic(&mut bag, CORRECTNESS, "% of committed updates surviving");
    let mut timed = Json::Obj(Vec::new());
    for name in timings::FAMILIES {
        let source = if *name == "transaction" {
            correctness.get("matrix").map(|_| correctness.clone())
        } else {
            bag.take_one(&[name])
        };
        let family =
            timings::normalize(name, source, throughput_trust).unwrap_or_else(|error| die(&error));
        timed.set(name, family);
    }
    let metrics = Json::obj(vec![
        ("throughput", throughput(&mut bag, throughput_trust)),
        ("rss", rss(&mut bag)),
        ("binary_size", deterministic(&mut bag, BINARY_SIZE, "KiB")),
        ("correctness", correctness),
        (
            "isolation",
            measured(&mut bag, ISOLATION, "commits/s", throughput_trust),
        ),
        ("timings", timed),
        ("allocs", deterministic(&mut bag, ALLOCS, "allocations/op")),
        ("scan_stream", deterministic(&mut bag, SCAN_STREAM, "B")),
        ("viability", deterministic(&mut bag, VIABILITY, "")),
    ]);
    let retractions = Json::Arr(bag.take_all(RETRACTION));

    if !bag.is_empty() {
        die(&format!(
            "unrecognized metric families: {}. They would be dropped from the run file. Accepted \
             names: {}",
            bag.names().join(", "),
            accepted_names().join(", ")
        ));
    }

    let run = Json::obj(vec![
        ("commit", Json::s(&args.commit)),
        ("label", Json::s(&args.label)),
        ("timestamp", Json::s(&run_meta::now_iso8601())),
        ("toolchain", Json::s(&run_meta::toolchain())),
        ("host", run_meta::host()),
        ("loadguard", guard.json()),
    ]);

    let doc = Json::obj(vec![
        ("schema_version", Json::Num(2.0)),
        ("run", run),
        ("metrics", metrics),
        ("plan_targets", run_meta::plan_targets()),
        ("retractions", retractions),
    ]);

    run_meta::write_new(&args.out, &json::to_string_pretty(&doc));
    summarize(&args.out, &doc);
}

struct Args {
    out: PathBuf,
    commit: String,
    label: String,
}

const USAGE: &str = "collect: assemble one schema v2 run file from every bench family.

  cargo bench --bench collect -- --out <run.json> --commit <sha> --label <label>

Families are read from $REGOLITH_BENCH_OUT (JSON Lines) when set, else ./bench-out/*.json.";

/// `None` means this invocation is not ours: `cargo bench` drives every bench
/// target with criterion's flags, and collect has nothing to do then.
fn parse_args() -> Option<Args> {
    let argv = common::args();
    let mut out: Option<PathBuf> = None;
    let mut commit: Option<String> = None;
    let mut label: Option<String> = None;
    let mut foreign: Vec<String> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        let value = |i: &mut usize, flag: &str| -> String {
            *i += 1;
            argv.get(*i)
                .cloned()
                .unwrap_or_else(|| die(&format!("{flag} needs a value")))
        };
        match argv[i].as_str() {
            "--out" => out = Some(PathBuf::from(value(&mut i, "--out"))),
            "--commit" => commit = Some(value(&mut i, "--commit")),
            "--label" => label = Some(value(&mut i, "--label")),
            "-h" | "--help" => {
                println!("{USAGE}");
                return None;
            }
            other => foreign.push(other.to_string()),
        }
        i += 1;
    }

    if out.is_none() {
        if foreign.is_empty() {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
        eprintln!(
            "collect: skipped, it assembles a run file and is not driven by `cargo bench` (saw \
             {}).",
            foreign.join(" ")
        );
        eprintln!(
            "  cargo bench --bench collect -- --out <run.json> --commit <sha> --label \
             <label>"
        );
        return None;
    }
    if !foreign.is_empty() {
        die(&format!("unrecognized arguments: {}", foreign.join(" ")));
    }

    Some(Args {
        out: out.unwrap_or_else(|| die("--out is required")),
        commit: commit.unwrap_or_else(|| die("--commit is required")),
        label: label.unwrap_or_else(|| die("--label is required")),
    })
}

fn accepted_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = WORKLOADS
        .iter()
        .flat_map(|(_, a)| a.iter().copied())
        .collect();
    for group in [
        SOAK,
        MEMORY,
        BINARY_SIZE,
        CORRECTNESS,
        ISOLATION,
        VIABILITY,
        RETRACTION,
        ALLOCS,
        SCAN_STREAM,
    ] {
        v.extend(group.iter().copied());
    }
    v.extend(
        timings::FAMILIES
            .iter()
            .copied()
            .filter(|name| *name != "transaction"),
    );
    v
}

fn throughput(bag: &mut Bag, trust: &str) -> Json {
    let mut workloads = Json::Obj(Vec::new());
    let mut reps: Vec<f64> = Vec::new();
    let mut measured = 0;
    for (slot, aliases) in WORKLOADS {
        match bag.take_one(aliases) {
            None => workloads.set(slot, absent(&[("points", Json::Arr(Vec::new()))])),
            Some(mut data) => {
                require_obj(&data, slot);
                if let Some(r) = data.remove("reps").and_then(|v| v.as_f64()) {
                    reps.push(r);
                }
                let trust = timings::runner_trust(&data, trust);
                apply_trust(&mut data, trust, slot);
                workloads.set(slot, data);
                measured += 1;
            }
        }
    }

    let mut family = Json::obj(vec![("unit", Json::s("ops/s"))]);
    if measured == 0 {
        family.set("trust", Json::s("absent"));
    }
    if let Some(first) = reps.first() {
        let agreed = reps.iter().all(|r| r == first);
        family.set(
            "reps",
            if agreed {
                Json::Num(*first)
            } else {
                Json::s("varies by workload")
            },
        );
    }
    family.set("workloads", workloads);
    family.set("criterion", records::criterion(trust));
    family
}

fn rss(bag: &mut Bag) -> Json {
    // RSS for a given configuration does not depend on how busy the host was,
    // so it stays clean whatever the load guard said.
    let trust = "clean";
    let mut workload: Option<String> = None;
    let mut entries = Vec::new();
    for mut s in bag.take_all(SOAK) {
        require_obj(&s, "soak");
        let variant = s
            .get("variant")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed")
            .to_string();
        match s
            .remove("workload")
            .and_then(|w| w.as_str().map(str::to_string))
        {
            None => die(&format!(
                "soak '{variant}' did not record the workload it ran"
            )),
            Some(w) => match &workload {
                None => workload = Some(w),
                Some(prev) if *prev == w => {}
                Some(_) => die(&format!(
                    "soak '{variant}' ran a different workload from the earlier soaks in this \
                     run; variants are only comparable when the workload is identical"
                )),
            },
        }
        apply_trust(&mut s, trust, &format!("soak '{variant}'"));
        entries.push(s);
    }

    let mut memory = bag.take_one(MEMORY);
    if let Some(m) = &memory {
        require_obj(m, "memory");
    }

    let mut family = Json::obj(vec![("unit", Json::s("MiB"))]);
    if let Some(w) = workload {
        family.set("workload", Json::Str(w));
    }
    // Either contributor is a measurement. `soak` is not in the bench
    // matrix, so deriving this from the soaks alone reported `absent`
    // over a memory bench that had run, which is a gap rendered where
    // there is data.
    let measured = !entries.is_empty() || memory.is_some();
    family.set("trust", Json::s(if measured { trust } else { "absent" }));
    family.set(
        "trust_note",
        Json::s(
            "RSS for a given configuration is deterministic and independent of host CPU load, so \
             it stays comparable across runs whatever the load guard said.",
        ),
    );
    family.set("soaks", Json::Arr(entries));

    for part in RSS_PARTS {
        match memory.as_mut().and_then(|m| m.remove(part)) {
            None => family.set(part, absent(&[])),
            Some(mut v) => {
                if v.is_obj() {
                    apply_trust(&mut v, trust, part);
                }
                family.set(part, v);
            }
        }
    }
    // Anything else the memory bench recorded is carried through, not dropped.
    if let Some(Json::Obj(extra)) = memory {
        for (k, v) in extra {
            family.set(&k, v);
        }
    }
    family
}

/// A family whose result a busy host cannot change: binary size, correctness,
/// viability. Trust is clean when it was collected and absent when it was not.
fn deterministic(bag: &mut Bag, aliases: &[&str], unit: &str) -> Json {
    measured(bag, aliases, unit, "clean")
}

fn measured(bag: &mut Bag, aliases: &[&str], unit: &str, trust: &str) -> Json {
    let name = aliases[0];
    match bag.take_one(aliases) {
        Some(mut data) => {
            require_obj(&data, name);
            let trust = if aliases == ISOLATION {
                timings::runner_trust(&data, trust)
            } else {
                trust
            };
            apply_trust(&mut data, trust, name);
            data
        }
        None => {
            let mut stub = Json::Obj(Vec::new());
            if !unit.is_empty() {
                stub.set("unit", Json::s(unit));
            }
            stub.set("trust", Json::s("absent"));
            stub.set("rows", Json::Arr(Vec::new()));
            stub
        }
    }
}

fn absent(extra: &[(&str, Json)]) -> Json {
    let mut v = Json::obj(vec![("trust", Json::s("absent"))]);
    for (k, e) in extra {
        v.set(k, e.clone());
    }
    v
}

/// Trust is only ever downgraded: a bench that declared its own doubt about a
/// measurement keeps it, and the load guard can add doubt but never remove it.
fn apply_trust(data: &mut Json, family_trust: &str, ctx: &str) {
    let declared = match data.get("trust") {
        None => "clean",
        Some(Json::Str(s)) => {
            let s = s.as_str();
            if !TRUST_LEVELS.contains(&s) {
                die(&format!(
                    "{ctx}: trust {s:?} is not one of {}",
                    TRUST_LEVELS.join(", ")
                ));
            }
            s
        }
        Some(other) => die(&format!(
            "{ctx}: trust must be a string, got {}",
            other.type_name()
        )),
    };
    let rank = |t: &str| TRUST_LEVELS.iter().position(|x| *x == t).unwrap_or(2);
    let worst = if rank(declared) >= rank(family_trust) {
        declared.to_string()
    } else {
        family_trust.to_string()
    };
    data.set("trust", Json::Str(worst));
}

fn require_obj(v: &Json, name: &str) {
    if !v.is_obj() {
        die(&format!(
            "family '{name}' emitted a {} where the schema wants an object",
            v.type_name()
        ));
    }
}

fn summarize(path: &std::path::Path, doc: &Json) {
    println!("collect: wrote {}", path.display());
    if let Some(Json::Obj(metrics)) = doc.get("metrics") {
        for (name, family) in metrics {
            let trust = family
                .get("trust")
                .and_then(|t| t.as_str())
                .unwrap_or("per-entry");
            println!("  {name:<12} trust={trust}{}", detail(family));
        }
    }
    if let Some(lg) = doc.get("run").and_then(|r| r.get("loadguard")) {
        let passed = matches!(lg.get("passed"), Some(Json::Bool(true)));
        println!("  loadguard    passed={passed}");
    }
}

fn detail(family: &Json) -> String {
    let count = |key: &str| {
        family
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    };
    let mut parts = Vec::new();
    if let Some(Json::Obj(w)) = family.get("workloads") {
        let measured = w
            .iter()
            .filter(|(_, v)| v.get("trust").and_then(|t| t.as_str()) != Some("absent"))
            .count();
        parts.push(format!("{measured}/{} workloads", w.len()));
    }
    for key in ["soaks", "rows", "benchmarks"] {
        if count(key) > 0 {
            parts.push(format!("{} {key}", count(key)));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  ({})", parts.join(", "))
    }
}
