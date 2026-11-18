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

const UNSCANNED_CATEGORIES: &[&str] = &[
    "follow_up_hook",
    "state_machine",
    "premise",
    "Proofs.Conformance.",
];

/// The call shapes that make a file a reader of a derived family, when they
/// follow the family's own path: `families::<key>::replay(…`.
const FAMILY_CALL_SPELLINGS: &[&str] = &[
    "::replay(",
    "::replay::<",
    "::replay)",
    "::cases(",
    "::cases::<",
    "::cases)",
];

/// The alias declaration that makes a file a reader of a derived family:
/// `families::<key> as family;`.
const FAMILY_ALIAS_DECLARATION: &str = " as family;";

/// The alias a file gives `families::<key>`, if it declares one.
///
/// `use gents_lean_vocab::families::<key> as <alias>;` binds the family to
/// `<alias>` for the rest of the file, so `<alias>::replay(` is as much a reader
/// as `families::<key>::replay(`. Resolved per family so a file that aliases one
/// family cannot lend that alias to another.
fn family_aliases(source: &str, key: &str, families: &BTreeSet<String>) -> Vec<String> {
    let needle = format!("families::{key} as ");
    let mut aliases = Vec::new();
    let mut from = 0;
    while let Some(found) = source[from..].find(&needle) {
        let start = from + found + needle.len();
        from = start;
        let rest = &source[start..];
        let end = rest.bytes().take_while(|byte| is_identifier(*byte)).count();
        if end == 0 {
            continue;
        }
        // A plain `use … as alias;` ends at a `;`; a re-export list or a nested
        // path carries something else there and is not an alias declaration.
        if !rest[end..].trim_start().starts_with(';') {
            continue;
        }
        let alias = &rest[..end];
        // An alias that is itself a family key would be indistinguishable from
        // the canonical spelling, so it is left to that branch.
        if families.contains(alias) {
            continue;
        }
        aliases.push(alias.to_owned());
    }
    aliases
}

/// How many readers of `key`'s family one file contains.
///
/// The canonical spellings count per occurrence, exactly as before. A file that
/// additionally binds the family to its own alias counts **once** for that
/// binding — not once per declaration and once per use, which would double every
/// file that writes both. `as family;` keeps counting through the canonical
/// spelling, so nothing about the existing counts changes.
fn reader_count(source: &str, key: &str, families: &BTreeSet<String>) -> usize {
    let alias_family = FAMILY_ALIAS_DECLARATION
        .trim_start_matches(" as ")
        .trim_end_matches(';');
    // A file is a reader when it either spells the family's path out or binds it
    // to an alias and uses that alias. The two are the *same* reader in one
    // file, so a file that does both counts once: counting the spellings and the
    // alias additively reported ordinary files as two readers.
    let spelled: usize = FAMILY_CALL_SPELLINGS
        .iter()
        .map(|site| source.matches(&format!("families::{key}{site}")).count())
        .sum();
    let declared = source
        .matches(&format!("families::{key}{FAMILY_ALIAS_DECLARATION}"))
        .count();
    // `as family;` is covered by `declared`; every other alias is covered here.
    let aliased = family_aliases(source, key, families)
        .into_iter()
        .filter(|alias| alias != alias_family)
        .any(|alias| alias_used(source, &alias));
    let any = spelled + declared > 0 || aliased;
    usize::from(any)
}

/// Whether `alias` is actually replayed or enumerated, as `<alias>::replay(`.
///
/// A bare `replay(` with no family path is not a reader of anything: the alias
/// has to be the receiver.
fn alias_used(source: &str, alias: &str) -> bool {
    FAMILY_CALL_SPELLINGS
        .iter()
        .any(|site| source.contains(&format!("{alias}{site}")))
}

/// The spellings `consumer_sites` accepts, for the failure message.
///
/// The message has to name them: B8's failure read "no native test reads these
/// emitted families" while a native test did, because it aliased the family
/// `as wire;` and the scan then only knew `as family;`.
fn accepted_spellings() -> String {
    let mut spellings: Vec<String> = FAMILY_CALL_SPELLINGS
        .iter()
        .map(|call| format!("  families::<key>{call}"))
        .collect();
    spellings.push(format!(
        "  families::<key>{}         (any other alias is counted too, when it is",
        FAMILY_ALIAS_DECLARATION
    ));
    spellings.push("     declared as `use …families::<key> as <alias>;` and then".to_owned());
    spellings.push(
        "     replayed as `<alias>::replay(…)` or enumerated as `<alias>::cases(…)`)".to_owned(),
    );
    spellings.join("\n")
}

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
        // A reader needs the family's path somewhere in the file: either spelled
        // out (`families::<key>…`) or bound to an alias first.
        if !source.contains("families::") && !source.contains(FAMILY_ALIAS_DECLARATION) {
            continue;
        }
        let prefix = format!("{DERIVED_FAMILIES}/");
        for (family, found) in sites.range_mut(prefix.clone()..) {
            let Some(key) = family.strip_prefix(&prefix) else {
                break;
            };
            let calls = reader_count(&source, key, families);
            found.extend((1..=calls).map(|call| format!("{relative}#{call}")));
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"),
        )
        .join("../..")
        .canonicalize()
        .expect("workspace root")
    }

    /// The families these tests scan for: one derived key, plus a sibling so an
    /// alias cannot leak between families.
    fn scan_families() -> BTreeSet<String> {
        [
            "families/alpha_family",
            "families/beta_family",
            "families/ordinary_family",
        ]
        .iter()
        .map(|family| (*family).to_owned())
        .collect()
    }

    /// An alias that is replayed or enumerated is a reader of that family.
    #[test]
    fn an_aliased_family_counts_as_a_reader() {
        let families = scan_families();
        for alias in ["wire", "family", "scenario"] {
            for call in ["::replay(", "::cases("] {
                let source = format!(
                    "use gents_lean_vocab::families::alpha_family as {alias};\n\
                     fn t() {{ {alias}{call}assert!(true) }}"
                );
                assert_eq!(
                    reader_count(&source, "alpha_family", &families),
                    1,
                    "`as {alias};` + `{alias}{call}` is one reader: {source}"
                );
                assert!(
                    family_aliases(&source, "alpha_family", &families).contains(&alias.to_owned())
                );
            }
        }
    }

    /// A declaration that never calls replay or cases reads nothing: binding a
    /// name is not reading a family.
    #[test]
    fn an_alias_without_a_call_is_not_a_reader() {
        let families = scan_families();
        for alias in ["wire", "scenario"] {
            let source = format!(
                "use gents_lean_vocab::families::alpha_family as {alias};\n\
                 fn t() {{ let _ = {alias}::RefusalInput::default(); }}"
            );
            assert_eq!(
                reader_count(&source, "alpha_family", &families),
                0,
                "an unused alias reads nothing: {source}"
            );
            // It is still recognised as an alias; it simply is not used.
            assert!(family_aliases(&source, "alpha_family", &families).contains(&alias.to_owned()));
        }
    }

    /// A bare `replay(` with no family path is nobody's reader. The alias must
    /// be the receiver, which is what separates this from widening the scan.
    #[test]
    fn a_bare_replay_call_is_not_a_reader() {
        let families = scan_families();
        for source in [
            "fn t() { replay(|| handle(input)); }".to_owned(),
            "fn t() { cases(|c| c.value()); }".to_owned(),
            "use gents_lean_vocab::families::alpha_family as wire;\n\
             fn t() { other::replay(|| handle(input)); }"
                .to_owned(),
        ] {
            assert_eq!(
                reader_count(&source, "alpha_family", &families),
                0,
                "no family path, no reader: {source}"
            );
        }
    }

    /// An alias belongs to the family it was declared for: the same name bound
    /// to `beta_family` must not read as a reader of `alpha_family`.
    #[test]
    fn an_alias_only_reads_the_family_it_is_declared_for() {
        let families = scan_families();
        let source = "use gents_lean_vocab::families::beta_family as wire;\n\
                      fn t() { wire::replay(|_| Ok(())); }";
        assert_eq!(reader_count(source, "beta_family", &families), 1);
        assert_eq!(
            reader_count(source, "alpha_family", &families),
            0,
            "beta's alias is not alpha's reader"
        );
    }

    /// The rule the ledger assertion relies on still holds with the alias
    /// spelling: one file is one reader, however many times it calls.
    #[test]
    fn one_alias_counts_once_however_many_calls() {
        let families = scan_families();
        let source = "use gents_lean_vocab::families::alpha_family as wire;\n\
                      fn a() { wire::replay(|_| Ok(())); }\n\
                      fn b() { wire::replay(|_| Ok(())); }\n\
                      fn c() { wire::cases(); }";
        assert_eq!(
            reader_count(source, "alpha_family", &families),
            1,
            "a declaration plus its uses is one reader, not four"
        );
    }

    /// A file that both spells the path out and binds it to an alias is still
    /// one file, so one reader. Counting the two spellings additively reported
    /// 18 ordinary files as two readers.
    #[test]
    fn a_file_with_a_canonical_call_and_an_alias_is_one_reader() {
        let families = scan_families();
        let source = "use gents_lean_vocab::families::alpha_family as locality;\n\
                      fn t() {\n\
                        let cases = gents_lean_vocab::families::alpha_family::cases();\n\
                        locality::replay(|_| Ok(()));\n\
                      }";
        assert_eq!(
            reader_count(source, "alpha_family", &families),
            1,
            "one file, both spellings, one reader"
        );
    }

    /// Two files read one family two different ways: an alias in one and the
    /// canonical spelling in the other count as two readers, which is what the
    /// "exactly one replay per derived family" rule detects.
    #[test]
    fn an_alias_and_a_canonical_spelling_are_two_readers() {
        let families = scan_families();
        let aliased = "use gents_lean_vocab::families::alpha_family as wire;\n\
                       fn t() { wire::replay(|_| Ok(())); }";
        let canonical = "fn t() { gents_lean_vocab::families::alpha_family::replay(|_| Ok(())); }";
        assert_eq!(reader_count(aliased, "alpha_family", &families), 1);
        assert_eq!(reader_count(canonical, "alpha_family", &families), 1);
        assert_eq!(
            reader_count(aliased, "alpha_family", &families)
                + reader_count(canonical, "alpha_family", &families),
            2,
            "two files, one family, two readers"
        );
        // And a file that does both spellings is not a third reader.
        let both = format!("{aliased}\n{canonical}");
        assert_eq!(
            reader_count(&both, "alpha_family", &families),
            1,
            "one file is one reader however it spells it"
        );
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
            "no native test reads these emitted families, and the coverage ledger does not say why: \
             {unread:?}\n\
             a native reader is a file containing, for the family's key `k`:\n\
             {spellings}",
            spellings = accepted_spellings()
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
            "a derived family is replayed by exactly one test: {misplaced:?}\n\
             a native reader is a file containing, for the family's key `k`:\n\
             {spellings}",
            spellings = accepted_spellings()
        );
    }
}
