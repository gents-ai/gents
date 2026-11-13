use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

const CONTRACT_METADATA: &[&str] = &[
    "coverage_ledger",
    "feature_matrix",
    "feature_surface_requirements",
];

const DOMAIN_FAMILIES: &[(&str, &str)] = &[];

const DERIVED_FAMILIES: &str = "families";
const DERIVED_VOCABULARIES: &str = "family_vocabularies";
const DERIVED_LEDGER_CATEGORY: &str = "family";

const UNSCANNED_CATEGORIES: &[&str] = &["follow_up_hook", "state_machine", "premise", "Proofs.Conformance."];

const SOURCE_ROOTS: &[&str] = &["crates", "apps", "packages"];
const SKIPPED_DIRECTORIES: &[&str] = &["target", "node_modules", "proofs", "dist", "gen"];

fn is_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn identifiers(source: &str) -> impl Iterator<Item = (u8, &str, u8)> {
    let bytes = source.as_bytes();
    let mut start = 0;
    std::iter::from_fn(move || {
        while start < bytes.len() && !is_identifier(bytes[start]) {
            start += 1;
        }
        if start == bytes.len() {
            return None;
        }
        let mut end = start;
        while end < bytes.len() && is_identifier(bytes[end]) {
            end += 1;
        }
        let before = if start == 0 { b' ' } else { bytes[start - 1] };
        let after = bytes.get(end).copied().unwrap_or(b' ');
        let identifier = &source[start..end];
        start = end;
        Some((before, identifier, after))
    })
}

fn families_read(
    source: &str,
    families: &BTreeSet<String>,
    loaders: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut read = BTreeSet::new();
    let mut quoted = BTreeSet::new();
    for (before, identifier, after) in identifiers(source) {
        let is_quoted = before == b'"' && after == b'"';
        if (before == b'.' || is_quoted) && families.contains(identifier) {
            read.insert(identifier.to_owned());
        }
        if is_quoted {
            quoted.insert(identifier);
        }
        if after == b'(' {
            if let Some(keys) = loaders.get(identifier) {
                read.extend(keys.iter().cloned());
            }
        }
    }
    for (key, _) in DOMAIN_FAMILIES {
        if read.contains(*key) {
            let domains = quoted.iter().map(|domain| format!("{key}/{domain}"));
            read.extend(domains.filter(|family| families.contains(family)));
        }
    }
    read
}

fn loaders(
    vocab_sources: &[String],
    families: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut direct = BTreeMap::new();
    let mut calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for source in vocab_sources {
        for function in source.split("\npub fn ").skip(1) {
            let body = function.split("\n}\n").next().unwrap_or(function);
            let name: String = body
                .bytes()
                .take_while(|byte| is_identifier(*byte))
                .map(char::from)
                .collect();
            if !name.starts_with("lean_") && !name.starts_with("assert_") {
                continue;
            }
            direct.insert(
                name.clone(),
                families_read(body, families, &BTreeMap::new()),
            );
            calls.insert(
                name.clone(),
                identifiers(&body[name.len()..])
                    .filter(|(_, callee, after)| *after == b'(' && *callee != name)
                    .map(|(_, callee, _)| callee.to_owned())
                    .collect(),
            );
        }
    }
    let mut resolved = direct.clone();
    loop {
        let mut changed = false;
        for (name, callees) in &calls {
            for callee in callees {
                let Some(inherited) = resolved.get(callee).cloned() else {
                    continue;
                };
                let own = resolved.get_mut(name).expect("loader is recorded");
                let before = own.len();
                own.extend(inherited);
                changed |= own.len() != before;
            }
        }
        if !changed {
            return resolved;
        }
    }
}

fn split_at_tests(source: &str) -> (&str, &str) {
    let first = [
        "#[cfg(test)]\nmod tests {",
        "#[cfg(test)]\nuse ",
        "#[test]",
        "#[tokio::test]",
    ]
    .iter()
    .filter_map(|marker| source.find(marker))
    .min();
    source.split_at(first.unwrap_or(source.len()))
}

fn source_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("source directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if !SKIPPED_DIRECTORIES.contains(&name) && !name.starts_with('.') {
                source_files(&path, files);
            }
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "ts" | "tsx")
        ) {
            files.push(path);
        }
    }
}

pub fn emitted_families(snapshot: &Value) -> BTreeSet<String> {
    let snapshot = snapshot.as_object().expect("snapshot object");
    let mut families: BTreeSet<String> = snapshot
        .iter()
        .filter(|(key, value)| {
            (value.is_array() || value.is_object())
                && !CONTRACT_METADATA.contains(&key.as_str())
                && ![DERIVED_FAMILIES, DERIVED_VOCABULARIES].contains(&key.as_str())
        })
        .map(|(key, _)| key.clone())
        .collect();
    for (key, _) in DOMAIN_FAMILIES {
        for entry in snapshot[*key].as_array().expect("domain families") {
            let domain = entry["domain"].as_str().expect("family domain");
            families.insert(format!("{key}/{domain}"));
        }
    }
    for derived in [DERIVED_FAMILIES, DERIVED_VOCABULARIES] {
        let derived = snapshot[derived].as_object().expect("derived families");
        families.extend(
            derived
                .keys()
                .map(|key| format!("{DERIVED_FAMILIES}/{key}")),
        );
    }
    families
}

pub fn ledger_family(category: &str, domain: &str) -> String {
    if category == DERIVED_LEDGER_CATEGORY {
        return format!("{DERIVED_FAMILIES}/{domain}");
    }
    match DOMAIN_FAMILIES
        .iter()
        .find(|(_, row_category)| *row_category == category)
    {
        Some((key, _)) => format!("{key}/{domain}"),
        None => category.to_owned(),
    }
}

pub fn consumer_sites(
    workspace: &Path,
    families: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let vocab = workspace.join("crates/gents-lean-vocab/src");
    let mut vocab_files = Vec::new();
    source_files(&vocab, &mut vocab_files);
    let vocab_sources: Vec<String> = vocab_files
        .iter()
        .map(|path| fs::read_to_string(path).expect("vocab source"))
        .collect();
    let loader_sources: Vec<String> = vocab_sources
        .iter()
        .map(|source| split_at_tests(source).0.to_owned())
        .collect();
    let loaders = loaders(&loader_sources, families);

    let mut sites: BTreeMap<String, BTreeSet<String>> = families
        .iter()
        .map(|family| (family.clone(), BTreeSet::new()))
        .collect();
    let mut files = Vec::new();
    for root in SOURCE_ROOTS {
        source_files(&workspace.join(root), &mut files);
    }
    for path in files {
        let relative = path
            .strip_prefix(workspace)
            .expect("workspace file")
            .display()
            .to_string();
        if relative.starts_with("crates/gents-lean-contract/src") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("workspace source");
        let source = if relative.starts_with("crates/gents-lean-vocab/src") {
            if relative.ends_with("consumer_scan.rs") {
                continue;
            }
            if relative.ends_with("/tests.rs") {
                source
            } else {
                split_at_tests(&source).1.to_owned()
            }
        } else {
            source
        };
        for family in families_read(&source, families, &loaders) {
            sites
                .get_mut(&family)
                .expect("known family")
                .insert(relative.clone());
        }
        if !source.contains("families::") {
            continue;
        }
        let prefix = format!("{DERIVED_FAMILIES}/");
        for (family, found) in sites.range_mut(prefix.clone()..) {
            let Some(key) = family.strip_prefix(&prefix) else {
                break;
            };
            let calls: usize = [
                "::replay(",
                "::replay::<",
                "::cases(",
                "::cases::<",
                " as family;",
            ]
            .iter()
            .map(|site| source.matches(&format!("families::{key}{site}")).count())
            .sum();
            found.extend((1..=calls).map(|call| format!("{relative}#{call}")));
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root")
    }

    #[test]
    fn every_emitted_family_has_a_consumer_or_a_ledger_reason() {
        let json = gents_lean_contract::load_contract_json().expect("Lean contract JSON");
        let snapshot: Value = serde_json::from_str(&json).expect("snapshot JSON");
        let families = emitted_families(&snapshot);
        let sites = consumer_sites(&workspace(), &families);

        let mut consumed = BTreeSet::new();
        let mut unconsumed = BTreeSet::new();
        let mut unknown = Vec::new();
        for row in &crate::lean_contract_snapshot().coverage_ledger {
            let family = ledger_family(&row.category, &row.domain);
            let names_a_family = families.contains(&family)
                || UNSCANNED_CATEGORIES
                    .iter()
                    .any(|prefix| row.category.starts_with(prefix));
            if !names_a_family {
                unknown.push(family.clone());
            }
            if row.consumed {
                consumed.insert(family);
            } else {
                unconsumed.insert(family);
            }
        }

        assert!(
            unknown.is_empty(),
            "coverage ledger rows name families the snapshot does not emit: {unknown:?}"
        );

        let unread: Vec<&str> = sites
            .iter()
            .filter(|(family, found)| {
                found.is_empty() && (consumed.contains(*family) || !unconsumed.contains(*family))
            })
            .map(|(family, _)| family.as_str())
            .collect();
        assert!(
            unread.is_empty(),
            "no native test reads these emitted families, and the coverage ledger does not say why: {unread:?}"
        );

        let prefix = format!("{DERIVED_FAMILIES}/");
        let misplaced: Vec<_> = sites
            .iter()
            .filter(|(family, found)| {
                family.starts_with(&prefix)
                    && found.len() != usize::from(!unconsumed.contains(*family))
            })
            .collect();
        assert!(
            misplaced.is_empty(),
            "a derived family is replayed by exactly one test: {misplaced:?}"
        );
    }
}
