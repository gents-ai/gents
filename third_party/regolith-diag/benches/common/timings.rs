//! Schema 2 timing rows shared by the comparator and both reports.
//!
//! Each row names one workload and its unit and direction. Observed ranges
//! are copied when the bench measured them, never inferred from a median.
//! The source stays alongside the rows so attempts and samples are retained.

use crate::json::Json;

/// Timing families whose workload shapes the assembler understands.
pub const FAMILIES: &[&str] = &[
    "transaction",
    "scan_commit",
    "append",
    "read_rules",
    "txn_callbacks",
    "sync",
];

/// Preserve a source record and expose its measured values as schema 2 rows.
/// Missing measurements stay null; trust can only be downgraded. An unknown
/// family or trust declaration is an error, never a certified measurement.
pub fn normalize(name: &str, source: Option<Json>, trust: &str) -> Result<Json, String> {
    if !FAMILIES.contains(&name) {
        return Err(format!("unknown timing family {name}"));
    }
    let Some(source) = source else {
        return Ok(Json::obj(vec![
            ("trust", Json::s("absent")),
            ("rows", Json::Arr(Vec::new())),
        ]));
    };
    if !matches!(source, Json::Obj(_) | Json::Arr(_)) {
        return Err(format!("{name}: expected an object or array"));
    }
    let declared = match source.get("trust") {
        None => "clean",
        Some(Json::Str(value)) => value.as_str(),
        Some(_) => return Err(format!("{name}: trust must be a string")),
    };
    let levels = ["clean", "contaminated", "absent"];
    let rank = |t: &str| levels.iter().position(|level| *level == t);
    let declared_rank =
        rank(declared).ok_or_else(|| format!("{name}: invalid trust {declared}"))?;
    let trust = runner_trust(&source, trust);
    let trust_rank = rank(trust).ok_or_else(|| format!("{name}: invalid trust {trust}"))?;
    let timing_trust = match source.get("timing_trust") {
        None => "clean",
        Some(Json::Str(value)) => value.as_str(),
        Some(_) => return Err(format!("{name}: timing_trust must be a string")),
    };
    let timing_rank =
        rank(timing_trust).ok_or_else(|| format!("{name}: invalid timing trust {timing_trust}"))?;
    let trust = levels[declared_rank.max(trust_rank).max(timing_rank)];
    let mut context = Json::Obj(Vec::new());
    for key in [
        "quick",
        "reps",
        "durability",
        "reads",
        "blocks",
        "ops_per_block",
    ] {
        if let Some(value) = source.get(key) {
            context.set(key, value.clone());
        }
    }
    let mut rows = Vec::new();
    match name {
        "transaction" => {
            for cell in array(source.get("matrix")) {
                let flavor = text(cell.get("flavor"));
                let level = text(cell.get("isolation"));
                if let Some(throughput) = cell.get("throughput") {
                    for shape in ["uncontended", "contended"] {
                        let key = format!("{flavor}/{level}/{shape}");
                        let mut row = measurement(
                            &key,
                            "commits/s",
                            true,
                            throughput.get(&format!("{shape}_commits_per_s")),
                        );
                        let mut context = Json::obj(vec![(
                            "threads",
                            if shape == "uncontended" {
                                Json::Num(1.0)
                            } else {
                                value(throughput.get("contended_threads"))
                            },
                        )]);
                        if shape == "contended" {
                            context.set("hot_keys", value(throughput.get("hot_keys")));
                        }
                        row.set("context", context);
                        let broken = cell
                            .get("correctness")
                            .and_then(|check| check.get("survival_pct_min"))
                            .and_then(Json::as_f64)
                            .is_some_and(|survival| survival < 100.0);
                        let failed = shape == "contended"
                            && throughput
                                .get("contended_attempts")
                                .is_some_and(|attempts| {
                                    ["abandoned", "engine_errors", "other_errors"].iter().any(
                                        |key| {
                                            attempts
                                                .get(key)
                                                .and_then(Json::as_f64)
                                                .is_some_and(|count| count > 0.0)
                                        },
                                    )
                                });
                        if broken || failed {
                            row.set(
                                "invalid_reason",
                                Json::s(
                                    "the workload lost updates or did not complete successfully",
                                ),
                            );
                        }
                        rows.push(row);
                    }
                }
            }
        }
        "append" => {
            for cell in array(source.get("append")) {
                let writers = cell.get("writers").and_then(Json::as_f64).unwrap_or(0.0);
                for shape in ["put", "append", "append_once"] {
                    let key = format!("{shape} @ {writers} writers");
                    rows.push(measurement(
                        &key,
                        "commits/s",
                        true,
                        cell.get("commits_per_s").and_then(|rates| rates.get(shape)),
                    ));
                }
            }
            if let Some(allocate) = source.get("allocate") {
                for (key, unit, higher) in [
                    ("latency_ns_p50", "ns", false),
                    ("latency_ns_p99", "ns", false),
                    ("allocations_per_s_8_callers", "allocations/s", true),
                ] {
                    rows.push(measurement(key, unit, higher, allocate.get(key)));
                }
            }
        }
        "scan_commit" => {
            for cell in source_rows(&source) {
                let keys = cell.get("keys").and_then(Json::as_f64).unwrap_or(0.0);
                for metric in ["commit_us", "scan_and_commit_us"] {
                    let key = format!("{}/{keys} keys/{metric}", text(cell.get("isolation")));
                    rows.push(measurement(&key, "us", false, cell.get(metric)));
                }
            }
        }
        "sync" => {
            for cell in array(source.get("benchmarks")) {
                let key = text(cell.get("benchmark"));
                rows.push(measurement(&key, "ns/iter", false, cell.get("median_ns")));
            }
        }
        "read_rules" => {
            for cell in source_rows(&source) {
                let key = text(cell.get("name"));
                let unit = text(cell.get("unit"));
                let mut row = measurement(&key, &unit, false, cell.get("value"));
                for bound in ["min", "max"] {
                    row.set(bound, value(cell.get(bound)));
                }
                rows.push(row);
            }
        }
        "txn_callbacks" => {
            for cell in source_rows(&source) {
                let key = text(cell.get("case"));
                let mut row = measurement(&key, "us", false, cell.get("us"));
                row.set("min", value(cell.get("min_us")));
                row.set("max", value(cell.get("max_us")));
                rows.push(row);
            }
        }
        _ => return Err(format!("unknown timing family {name}")),
    }
    Ok(Json::obj(vec![
        (
            "trust",
            Json::s(if rows.is_empty() { "absent" } else { trust }),
        ),
        ("context", context),
        ("rows", Json::Arr(rows)),
        ("measurement", value(source.get("measurement"))),
        ("source", source),
    ]))
}

fn array(value: Option<&Json>) -> &[Json] {
    value.and_then(Json::as_array).unwrap_or(&[])
}

/// The guard belongs to the runner that measured the family. Collection's
/// guard is a fallback for records emitted before per-runner provenance.
pub fn runner_trust<'run>(source: &Json, fallback: &'run str) -> &'run str {
    match source.get("measurement") {
        Some(measurement) => match measurement.get("guard_passed") {
            Some(Json::Bool(true)) => "clean",
            _ => "contaminated",
        },
        None => fallback,
    }
}

fn source_rows(source: &Json) -> &[Json] {
    source
        .as_array()
        .unwrap_or_else(|| array(source.get("rows")))
}

fn text(value: Option<&Json>) -> String {
    value
        .and_then(Json::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn value(value: Option<&Json>) -> Json {
    value.cloned().unwrap_or(Json::Null)
}

fn measurement(key: &str, unit: &str, higher: bool, data: Option<&Json>) -> Json {
    let median = match data {
        Some(Json::Num(n)) => Json::Num(*n),
        _ => value(data.and_then(|data| data.get("median"))),
    };
    Json::obj(vec![
        ("key", Json::s(key)),
        ("name", Json::s(key)),
        ("unit", Json::s(unit)),
        ("higher_is_better", Json::Bool(higher)),
        ("median", median),
        ("min", value(data.and_then(|data| data.get("min")))),
        ("max", value(data.and_then(|data| data.get("max")))),
    ])
}
