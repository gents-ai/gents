//! Formats scanner output (`FileCandidates`) into the model-facing candidates
//! block consumed by the pack's runner prompt.
//!
//! The header (`files:`/`candidates:`/`slugs:`) and per-file counters are
//! always computed over the full, uncapped result set — `max_chars` only
//! governs how much evidence (matched-line excerpts) makes it into the
//! payload text. Once a full per-file block would push the payload past the
//! cap, remaining files are demoted to single inventory lines so every file
//! is still named even when its evidence is dropped.

use std::fmt::Write as _;

use super::{FileCandidates, NoiseTier};

#[derive(Debug, Clone)]
pub(crate) struct ScanOutput {
    pub payload: String,                   // the model-facing candidates block
    pub candidate_total: usize,            // total matches across all files
    pub candidate_files: usize,            // files with >=1 match
    pub slug_counts: Vec<(String, usize)>, // sorted by count desc, then slug
    pub overflow_count: usize,             // files demoted to path-only lines
}

/// Minimum (best) tier among a file's matches. Panics on an empty match list
/// — callers only pass `FileCandidates` produced by `scan_root`/`match_content`,
/// which never emit empty entries.
fn best_tier(file: &FileCandidates) -> NoiseTier {
    file.matches
        .iter()
        .map(|m| m.tier)
        .min()
        .expect("FileCandidates must have at least one match")
}

fn slug_list(file: &FileCandidates) -> String {
    let mut slugs: Vec<&str> = file.matches.iter().map(|m| m.slug).collect();
    slugs.sort_unstable();
    slugs.dedup();
    slugs.join(",")
}

fn write_full_block(out: &mut String, file: &FileCandidates) {
    let _ = writeln!(out, "{}", file.path);
    for m in &file.matches {
        // Pad the bracketed tier tag (not the label) to align slugs across
        // rows: "[precise] " and "[normal]  " both land at column 10.
        let tag = format!("[{}]", m.tier.label());
        let _ = writeln!(out, "  {:<10}{} L{}: {}", tag, m.slug, m.line, m.excerpt);
    }
}

fn write_inventory_line(out: &mut String, file: &FileCandidates) {
    let _ = writeln!(out, "{} (slugs: {})", file.path, slug_list(file));
}

pub(crate) fn format_payload(files: &[FileCandidates], max_chars: usize) -> ScanOutput {
    let candidate_total: usize = files.iter().map(|f| f.matches.len()).sum();
    let candidate_files = files.len();

    let mut slug_counts_map: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for file in files {
        for m in &file.matches {
            *slug_counts_map.entry(m.slug).or_insert(0) += 1;
        }
    }
    let mut slug_counts: Vec<(String, usize)> = slug_counts_map
        .into_iter()
        .map(|(slug, count)| (slug.to_string(), count))
        .collect();
    slug_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // Also compute per-slug best tier for the header's `slug=count(tier)` display.
    let mut slug_tier_map: std::collections::HashMap<&str, NoiseTier> = std::collections::HashMap::new();
    for file in files {
        for m in &file.matches {
            slug_tier_map
                .entry(m.slug)
                .and_modify(|t| *t = (*t).min(m.tier))
                .or_insert(m.tier);
        }
    }

    let mut sorted_files: Vec<&FileCandidates> = files.iter().collect();
    sorted_files.sort_by(|a, b| best_tier(a).cmp(&best_tier(b)).then_with(|| a.path.cmp(&b.path)));

    let mut header = String::new();
    let _ = writeln!(
        header,
        "files: {candidate_files}  candidates: {candidate_total}"
    );
    let slugs_line: Vec<String> = slug_counts
        .iter()
        .map(|(slug, count)| {
            let tier = slug_tier_map.get(slug.as_str()).copied().unwrap_or(NoiseTier::Noisy);
            format!("{slug}={count}({})", tier.label())
        })
        .collect();
    let _ = writeln!(header, "slugs: {}", slugs_line.join(" "));

    let mut payload = header;
    let mut overflow_count = 0usize;
    let mut in_overflow = false;

    for file in &sorted_files {
        if in_overflow {
            // Already demoted; every remaining file is inventory-only.
            write_inventory_line(&mut payload, file);
            overflow_count += 1;
            continue;
        }

        let mut candidate_block = String::new();
        write_full_block(&mut candidate_block, file);

        if payload.len() + candidate_block.len() > max_chars {
            // Demote this file and all remaining files to inventory lines.
            in_overflow = true;
            write_inventory_line(&mut payload, file);
            overflow_count += 1;
        } else {
            payload.push_str(&candidate_block);
        }
    }

    ScanOutput {
        payload,
        candidate_total,
        candidate_files,
        slug_counts,
        overflow_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{CandidateMatch, FileCandidates};
    use super::super::matchers::NoiseTier;

    fn file(path: &str, slug: &'static str, tier: NoiseTier, lines: usize) -> FileCandidates {
        FileCandidates {
            path: path.to_string(),
            matches: (1..=lines).map(|line| CandidateMatch {
                slug, tier, line,
                excerpt: format!("let x = {line}; // {}", "y".repeat(80)),
            }).collect(),
        }
    }

    #[test]
    fn full_payload_sorts_precise_first_and_counts() {
        let files = vec![
            file("z/noisy.rs", "path-traversal", NoiseTier::Noisy, 1),
            file("a/precise.rs", "graphql-injection", NoiseTier::Precise, 2),
        ];
        let out = format_payload(&files, 100_000);
        assert_eq!(out.candidate_total, 3);
        assert_eq!(out.candidate_files, 2);
        assert_eq!(out.overflow_count, 0);
        let precise_pos = out.payload.find("a/precise.rs").unwrap();
        let noisy_pos = out.payload.find("z/noisy.rs").unwrap();
        assert!(precise_pos < noisy_pos, "precise files must sort first");
        assert!(out.slug_counts.iter().any(|(s, n)| s == "graphql-injection" && *n == 2));
    }

    #[test]
    fn cap_demotes_to_inventory_and_counts_overflow() {
        let files: Vec<FileCandidates> = (0..50)
            .map(|i| file(&format!("src/f{i:02}.rs"), "secret-in-log", NoiseTier::Normal, 3))
            .collect();
        let generous = format_payload(&files, 1_000_000);
        let tight = format_payload(&files, generous.payload.len() / 4);
        assert!(tight.overflow_count > 0, "tight cap must demote some files");
        // Inventory is complete: every file path still appears.
        for i in 0..50 {
            let path = format!("src/f{i:02}.rs");
            assert!(tight.payload.contains(&path), "missing inventory for {path}");
        }
        // Counters are cap-independent.
        assert_eq!(tight.candidate_total, generous.candidate_total);
    }
}
