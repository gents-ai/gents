//! The input side of the run file: metric families the benches emitted, and
//! criterion's own per-iteration estimates.
//!
//! Included by collect.rs with `#[path]`, alongside `json` and `run_meta` at
//! the crate root.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::json::{self, Json};
use crate::run_meta::die;

/// Every family record read this run, in the order the benches wrote them.
/// Families are drained by name as the run file is assembled; whatever is left
/// at the end is a name the assembler does not know, which is an error rather
/// than a silent drop.
pub struct Bag(pub Vec<(String, Json)>);

impl Bag {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.0.iter().map(|(n, _)| n.as_str()).collect()
    }

    pub fn take_all(&mut self, aliases: &[&str]) -> Vec<Json> {
        let mut hit = Vec::new();
        let mut rest = Vec::new();
        for (name, data) in self.0.drain(..) {
            if aliases.contains(&name.as_str()) {
                hit.push(data);
            } else {
                rest.push((name, data));
            }
        }
        self.0 = rest;
        hit
    }

    pub fn take_one(&mut self, aliases: &[&str]) -> Option<Json> {
        let mut hit = self.take_all(aliases);
        match hit.len() {
            0 => None,
            1 => Some(hit.remove(0)),
            n => die(&format!(
                "family '{}' was emitted {n} times; a run file records one measurement per \
                 family. Collect against a fresh output file.",
                aliases[0]
            )),
        }
    }
}

/// `$REGOLITH_BENCH_OUT` when the run was collected into one JSON Lines file,
/// otherwise whatever a standalone bench left in `./bench-out/`.
pub fn record_files() -> Vec<PathBuf> {
    match std::env::var_os("REGOLITH_BENCH_OUT") {
        Some(p) if !p.is_empty() => vec![PathBuf::from(p)],
        _ => {
            let mut files: Vec<PathBuf> = match fs::read_dir("bench-out") {
                Ok(rd) => rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "json"))
                    .collect(),
                Err(_) => Vec::new(),
            };
            files.sort();
            files
        }
    }
}

pub fn describe(files: &[PathBuf]) -> String {
    if files.is_empty() {
        "$REGOLITH_BENCH_OUT (unset) and ./bench-out/*.json (empty or missing)".to_string()
    } else {
        files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn read_records(files: &[PathBuf]) -> Bag {
    let mut out = Vec::new();
    for f in files {
        let text =
            fs::read_to_string(f).unwrap_or_else(|e| die(&format!("read {}: {e}", f.display())));
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let at = format!("{}:{}", f.display(), n + 1);
            let record =
                json::parse(line).unwrap_or_else(|e| die(&format!("{at}: malformed JSON: {e}")));
            let family = match record.get("family").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => die(&format!("{at}: record has no \"family\" string")),
            };
            let data = match record.get("data") {
                Some(d) => d.clone(),
                None => die(&format!("{at}: family '{family}' has no \"data\"")),
            };
            out.push((family, data));
        }
    }
    Bag(out)
}

/// Criterion's own estimates, folded in as supporting per-iteration timings
/// beside the ops/s the throughput benches report themselves.
pub fn criterion(trust: &str) -> Json {
    criterion_since(trust, None, "")
}

/// Estimates produced by this invocation of one Criterion family. Earlier
/// output and other benchmark groups must not become this run's samples.
pub fn criterion_since(trust: &str, since: Option<SystemTime>, prefix: &str) -> Json {
    let root = PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into()))
        .join("criterion");
    let mut found = Vec::new();
    walk(&root, &root, 0, since, &mut found);
    found.retain(|(id, _, _)| id.starts_with(prefix));
    found.sort_by(|a, b| a.0.cmp(&b.0));

    let benchmarks: Vec<Json> = found
        .into_iter()
        .map(|(id, mean, median)| {
            Json::obj(vec![
                ("benchmark", Json::Str(id)),
                ("mean_ns", Json::Num(mean)),
                ("median_ns", Json::Num(median)),
            ])
        })
        .collect();
    let trust = if benchmarks.is_empty() {
        "absent"
    } else {
        trust
    };
    Json::obj(vec![
        ("unit", Json::s("ns/iter")),
        ("trust", Json::s(trust)),
        (
            "source",
            Json::Str(format!("{}/**/new/estimates.json", root.display())),
        ),
        ("benchmarks", Json::Arr(benchmarks)),
    ])
}

fn walk(
    dir: &Path,
    root: &Path,
    depth: usize,
    since: Option<SystemTime>,
    out: &mut Vec<(String, f64, f64)>,
) {
    if depth > 8 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut subdirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    for sub in subdirs {
        // Criterion keeps the newest sample set under `new/`; `base/` and any
        // saved baseline beside it are older copies of the same benchmark.
        if sub.file_name().is_some_and(|n| n == "new") {
            let est = sub.join("estimates.json");
            let fresh = since.is_none_or(|since| {
                fs::metadata(&est)
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(|modified| modified >= since)
            });
            if est.is_file() && fresh {
                let id = sub
                    .parent()
                    .and_then(|p| p.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let (mean, median) = estimates(&est);
                out.push((id, mean, median));
            }
            continue;
        }
        walk(&sub, root, depth + 1, since, out);
    }
}

fn estimates(path: &Path) -> (f64, f64) {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| die(&format!("read {}: {e}", path.display())));
    let v = json::parse(&text).unwrap_or_else(|e| die(&format!("{}: {e}", path.display())));
    let point = |k: &str| {
        v.get(k)
            .and_then(|x| x.get("point_estimate"))
            .and_then(|x| x.as_f64())
            .unwrap_or_else(|| die(&format!("{}: no {k}.point_estimate", path.display())))
    };
    (point("mean"), point("median"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn earlier_criterion_output_is_not_a_sample_of_this_invocation() {
        use super::*;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("sync_uncontended/mutex/new");
        fs::create_dir_all(&new).unwrap();
        let path = new.join("estimates.json");
        fs::write(
            &path,
            r#"{"mean":{"point_estimate":5},"median":{"point_estimate":4}}"#,
        )
        .unwrap();
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        let mut rows = Vec::new();
        walk(dir.path(), dir.path(), 0, Some(modified), &mut rows);
        assert_eq!(rows, vec![("sync_uncontended/mutex".to_string(), 5.0, 4.0)]);
        rows.clear();
        walk(
            dir.path(),
            dir.path(),
            0,
            Some(modified + Duration::from_secs(1)),
            &mut rows,
        );
        assert!(rows.is_empty(), "an earlier invocation's sample was reused");
    }
}
