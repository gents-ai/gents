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

/// The pre-existing alias, `families::<key> as family;`, which counts as a reader
/// per declaration and is not also counted through its uses.
const CONVENTIONAL_ALIAS: &str = "family";

/// The language a scanned file is written in: `.rs` and `.ts` differ in what a
/// string means to this scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLanguage {
    Rust,
    TypeScript,
}

impl SourceLanguage {
    /// The language of a scanned file.
    ///
    /// Explicit, and loud on anything else: an extension added to
    /// `source_files` without a decision here would otherwise be silently read
    /// by whichever rules the catch-all happened to pick.
    fn of(path: &std::path::Path) -> Self {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("rs") => SourceLanguage::Rust,
            Some("ts" | "tsx") => SourceLanguage::TypeScript,
            other => panic!(
                "consumer_scan has no language for extension {other:?} ({}); \
                 add it to SourceLanguage::of and to source_files",
                path.display()
            ),
        }
    }
}

/// The offset of the `r` and the hash count of a Rust raw string at `i`.
///
/// `r"…"`, `r#"…"#`, `br##"…"##`. Three conditions, each of which an earlier
/// version missed: the `r` must begin a token (`r#type`, `r#try` and `r#fn` are
/// identifiers); the hashes must be followed by the opening `"` (those
/// identifiers have none); and the hash count is read from the `r`, not from the
/// token start, where a `b` prefix would shift it by one.
fn raw_string_opening(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if i > 0 && is_identifier(bytes[i - 1]) {
        return None;
    }
    let mut at = i;
    if bytes.get(at) == Some(&b'b') {
        if bytes.get(at + 1) != Some(&b'r') {
            return None;
        }
        at += 1;
    } else if bytes.get(at) != Some(&b'r') {
        return None;
    }
    let mut j = at + 1;
    let hashes = bytes[j..].iter().take_while(|byte| **byte == b'#').count();
    j += hashes;
    (bytes.get(j) == Some(&b'"')).then_some((at, hashes))
}

/// Strip `//` line comments, `/* … */` blocks and string/char/raw-string literals, by Rust rules.
///
/// The scan is a source-text heuristic, so a commented-out `use … as wire;` or a
/// doc-comment example would otherwise read as a declaration, and text inside a
/// string as a call. Replaced with a space of the same length so byte offsets and
/// word boundaries stay meaningful.
fn strip_rust_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        // A raw string, r"…" / r#"…"# / br#"…"#. `raw_string_opening` requires the
        // `r` to begin a token, the hashes to be followed by `"`, and it reports
        // the hash count from the `r` rather than from the token start.
        if let Some((j, hashes)) = raw_string_opening(bytes, i) {
            let opening = j + 1 + hashes;
            let closer = format!("\"{}", "#".repeat(hashes));
            out.push(' ');
            i = opening + 1;
            if let Some(found) = source[i..].find(&closer) {
                let end = i + found + closer.len();
                out.extend(std::iter::repeat_n(' ', end - i));
                i = end;
            } else {
                out.extend(std::iter::repeat_n(' ', source.len() - i));
                i = source.len();
            }
            continue;
        }
        match (bytes[i], bytes.get(i + 1).copied()) {
            // A line comment, up to and excluding the newline.
            (b'/', Some(b'/')) => {
                let end = source[i..]
                    .find('\n')
                    .map_or(source.len(), |found| i + found);
                out.extend(std::iter::repeat_n(' ', end - i));
                i = end;
            }
            // A block comment, possibly nested one level by Rust's rules; the
            // simple form is enough here, and an unclosed one swallows the rest.
            (b'/', Some(b'*')) => {
                let end = source[i + 2..]
                    .find("*/")
                    .map_or(source.len(), |found| i + 2 + found + 2);
                out.extend(std::iter::repeat_n(' ', end - i));
                i = end;
            }
            // A string literal; an escaped quote inside does not close it.
            (b'"', _) => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'"' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                out.extend(std::iter::repeat_n(' ', j - i));
                i = j;
            }
            // A char literal, `'\''` / `'a'`, vs a lifetime `'a` or a stray
            // apostrophe in prose. A literal is one character or one escape and
            // nothing else; the old six-byte lookahead closed on a *later*
            // apostrophe and blanked everything between, so a `"the user's"`
            // in a `format!` string swallowed the `families::` path 70 bytes on.
            (b'\'', _) => {
                let mut closed = None;
                let mut j = i + 1;
                if bytes.get(j) == Some(&b'\\') {
                    j += 2;
                    while j < bytes.len() && j <= i + 12 {
                        match bytes[j] {
                            b'\'' => {
                                closed = Some(j + 1);
                                break;
                            }
                            b'\n' => break,
                            _ => j += 1,
                        }
                    }
                } else if let Some(ch) = source[j..].chars().next() {
                    if ch != '\'' && ch != '\n' {
                        j += ch.len_utf8();
                        if bytes.get(j) == Some(&b'\'') {
                            closed = Some(j + 1);
                        }
                    }
                }
                match closed {
                    Some(end) => {
                        out.extend(std::iter::repeat_n(' ', end - i));
                        i = end;
                    }
                    None => {
                        out.push('\'');
                        i += 1;
                    }
                }
            }
            _ => {
                let ch = source[i..].chars().next().expect("a char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The offset just past the closing delimiter of a quoted run starting at `i`.
///
/// An escaped delimiter inside does not close it.
///
/// A **backtick** template literal spans lines: JavaScript allows one to contain
/// newlines, and a `//` inside it is text rather than a comment. Single and double
/// quotes do not span lines — they stop at the end of theirs, so a stray quote is
/// a local mistake instead of blanking the remainder of the file.
fn quoted_end(source: &str, i: usize, delimiter: u8) -> usize {
    let bytes = source.as_bytes();
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            byte if byte == delimiter => return j + 1,
            b'\n' if delimiter != b'`' => return j,
            _ => j += 1,
        }
    }
    source.len()
}

/// Strip `//` and `/* */` comments and keep string literals.
///
/// A `.ts` conformance table names its reader inside a string
/// (`"families::<key>::cases()"`), so for those files only comments go — the
/// strings stay, and a spelling inside one counts. A backtick template literal
/// is one string across its newlines, so a `//`-led line inside it is text, not
/// a comment.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        // A quoted run first, so `//` inside a string is not a comment and a
        // template literal is copied through whole. A template's contents are a
        // string, so a spelling inside one must not count.
        let delimiter = match bytes[i] {
            b'"' | b'`' | b'\'' => Some(bytes[i]),
            _ => None,
        };
        if let Some(delimiter) = delimiter {
            let end = quoted_end(source, i, delimiter);
            out.push_str(&source[i..end]);
            i = end;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            let end = source[i..]
                .find('\n')
                .map_or(source.len(), |found| i + found);
            out.extend(std::iter::repeat_n(' ', end - i));
            i = end;
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let end = source[i + 2..]
                .find("*/")
                .map_or(source.len(), |found| i + 2 + found + 2);
            out.extend(std::iter::repeat_n(' ', end - i));
            i = end;
            continue;
        }
        // By `char`, not by byte: `bytes[i] as char` would turn each byte of a
        // non-ASCII character into its own Latin-1 character and mangle the copy.
        let ch = source[i..].chars().next().expect("a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Every alias this file gives `families::<key>`, from a standalone
/// `use …families::<key> as <alias>;`.
///
/// Grouped imports (`use …families::{<key> as <alias>};`) are not recognised;
/// `accepted_spellings` says so. An alias that is itself a family key is ignored,
/// because it is indistinguishable from the canonical spelling.
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
        // A standalone `use … as alias;` ends at `;`; a re-export list or a
        // nested path carries something else there.
        if !rest[end..].trim_start().starts_with(';') {
            continue;
        }
        let alias = &rest[..end];
        if families.contains(alias) {
            continue;
        }
        aliases.push(alias.to_owned());
    }
    aliases
}

/// Every alias in the file, with the families it names.
///
/// The family set is keyed `families/<domain>`, while a `use` names the bare
/// `<domain>`, so the prefix is stripped before matching.
fn alias_declarations(
    source: &str,
    families: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let prefix = format!("{DERIVED_FAMILIES}/");
    let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for family in families {
        let Some(key) = family.strip_prefix(&prefix) else {
            continue;
        };
        for alias in family_aliases(source, key, families) {
            declared.entry(alias).or_default().insert(family.clone());
        }
    }
    declared
}

/// How many occurrences of `<alias>::replay(` / `::cases(` the file has.
///
/// The alias must stand on its own: the byte before it is neither an identifier
/// character nor `:`, so `rewire::replay(` and `crate::wire::replay(` are not
/// uses of `wire`.
fn alias_use_count(source: &str, alias: &str) -> usize {
    let bytes = source.as_bytes();
    let mut total = 0;
    let mut from = 0;
    while let Some(found) = source[from..].find(alias) {
        let at = from + found;
        from = at + alias.len();
        let boundary = at > 0 && {
            let before = bytes[at - 1];
            !is_identifier(before) && before != b':'
        };
        if !boundary && at != 0 {
            continue;
        }
        let mut rest = &source[at + alias.len()..];
        // Trimmed so `<alias> ::replay(` still reads, as Rust allows.
        if rest.starts_with(char::is_whitespace) {
            rest = rest.trim_start();
        }
        if FAMILY_CALL_SPELLINGS
            .iter()
            .any(|site| rest.starts_with(site))
        {
            total += 1;
        }
    }
    total
}

/// How many readers of `key`'s family one file contains: one per occurrence.
///
/// Counting is per occurrence because a file holds many tests, and the rule the
/// ledger enforces is "exactly one *replay* per derived family" — two replays in
/// one file must stay visible as two.
///
/// * a canonical `families::<key>::replay(` / `::cases(` — one per occurrence;
/// * `families::<key> as family;` — one per declaration, and not also through
///   `family::replay(` (its pre-existing behaviour);
/// * `<alias>::replay(` / `::cases(` for any other alias — one per use;
/// * the alias *declaration* itself — nothing.
///
/// Returns `Err` when one alias names two families in this file: the uses cannot
/// say which is meant, and crediting both would let a family nobody replays be
/// satisfied by a sibling's call.
#[cfg(test)]
fn reader_count(source: &str, key: &str, families: &BTreeSet<String>) -> Result<usize, String> {
    reader_count_in(source, key, families, SourceLanguage::Rust)
}

/// As [`reader_count`], for a file of a given language.
#[cfg(test)]
fn reader_count_in(
    source: &str,
    key: &str,
    families: &BTreeSet<String>,
    language: SourceLanguage,
) -> Result<usize, String> {
    reader_count_prepared(&prepare(source, families, language), key)
}

/// The per-file work `reader_count` needs, done once however many families are
/// counted for this file.
///
/// Stripping comments and resolving aliases is O(source); doing that per family
/// made the scan O(files x families) over 1205 families, and the full-lib test
/// ran for over twenty minutes.
struct PreparedSource {
    /// The source with comments, strings and char literals blanked.
    stripped: String,
    /// The text canonical spellings are counted in: `stripped` for Rust, a
    /// comments-only copy for TypeScript.
    spellings: String,
    /// Each alias in the file, the families it names, and its use count.
    aliases: Vec<(String, BTreeSet<String>, usize)>,
}

fn prepare(source: &str, families: &BTreeSet<String>, language: SourceLanguage) -> PreparedSource {
    let stripped = strip_rust_comments_and_strings(source);
    let aliases = alias_declarations(&stripped, families)
        .into_iter()
        .map(|(alias, keys)| {
            let uses = alias_use_count(&stripped, &alias);
            (alias, keys, uses)
        })
        .collect();
    // The review's rule, exactly: `.ts`/`.tsx` count canonical spellings in a
    // comments-stripped, strings-kept copy, because a conformance table names its
    // reader in a string. `.rs` counts in `stripped`, so a Rust string — a panic
    // message, a fixture source — is not a reader. Nothing is "restored" from a
    // difference against the raw text, which previously undid the stripping for
    // comments as well.
    let spellings = match language {
        SourceLanguage::TypeScript => strip_comments(source),
        SourceLanguage::Rust => stripped.clone(),
    };
    PreparedSource {
        stripped,
        spellings,
        aliases,
    }
}

fn reader_count_prepared(prepared: &PreparedSource, key: &str) -> Result<usize, String> {
    let spellings = &prepared.spellings;
    let mut count: usize = FAMILY_CALL_SPELLINGS
        .iter()
        .map(|site| spellings.matches(&format!("families::{key}{site}")).count())
        .sum();
    // The conventional alias, per declaration. Its uses are not counted too.
    count += spellings
        .matches(&format!("families::{key} as {CONVENTIONAL_ALIAS};"))
        .count();

    for (alias, keys, uses) in &prepared.aliases {
        if *uses == 0 {
            continue;
        }
        // `as family;` counts per declaration and ignores its uses, so it has
        // no attribution to lose and is exempt from the ambiguity rule.
        if alias != CONVENTIONAL_ALIAS && keys.len() > 1 {
            return Err(format!(
                "ambiguous alias `{alias}`: this file declares it for {keys:?}, so its \\
                 {uses} use(s) cannot be attributed; give each family its own alias"
            ));
        }
        let wanted = format!("{DERIVED_FAMILIES}/{key}");
        if keys.contains(&wanted) && alias != CONVENTIONAL_ALIAS {
            count += *uses;
        }
    }
    Ok(count)
}

/// Whether `alias` is actually replayed or enumerated, as `<alias>::replay(`.
#[cfg(test)]
fn alias_used(source: &str, alias: &str) -> bool {
    alias_use_count(source, alias) > 0
}

/// The spellings `consumer_sites` accepts, for the failure message.
///
/// The message has to name them: B8's failure read "no native test reads these
/// emitted families" while a native test did, because it aliased the family
/// `as wire;` and the scan then only knew `as family;`.
fn accepted_spellings() -> String {
    let mut spellings: Vec<String> = FAMILY_CALL_SPELLINGS
        .iter()
        .map(|call| format!("    families::<key>{call}"))
        .collect();
    spellings.push(format!("    families::<key> as {CONVENTIONAL_ALIAS};"));
    spellings.push(String::new());
    spellings.push("  Counting rules:".to_owned());
    spellings.push("    * one per occurrence — a file may hold many tests, and each".to_owned());
    spellings.push("      occurrence counts, so two replays in one file read as two;".to_owned());
    spellings
        .push("    * the `as family;` declaration counts once per declaration, and its".to_owned());
    spellings.push("      uses are not counted again.".to_owned());
    spellings.push(String::new());
    spellings.push("  A standalone use of any other alias also reads the family:".to_owned());
    spellings.push("    use gents_lean_vocab::families::<key> as <alias>;".to_owned());
    spellings.push("    <alias>::replay(…)   or   <alias>::cases(…)".to_owned());
    spellings
        .push("  The alias declaration itself counts nothing; each use counts one.".to_owned());
    spellings.push(String::new());
    spellings.push("  Not recognised, and they do not read the family:".to_owned());
    spellings.push("    * a grouped import, `families::{{<key> as <alias>}};`;".to_owned());
    spellings.push("    * an alias equal to a family key, which is ignored;".to_owned());
    spellings.push("    * a bare `replay(` with no family path or alias receiver;".to_owned());
    spellings
        .push("    * `rewire::replay(` or `crate::wire::replay(` for alias `wire`,".to_owned());
    spellings.push("      which need the alias at a word boundary;".to_owned());
    spellings.push("    * anything inside a comment or a string literal.".to_owned());
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
        // `families_read` above has already recorded a reader reached by a
        // loader or a domain string. This loop only adds the count from the
        // `families::<key>` spellings and aliases, so a file with no such path
        // has nothing to add and must not have its count overwritten with zero.
        if !source.contains("families::") {
            continue;
        }
        let prefix = format!("{DERIVED_FAMILIES}/");
        let language = SourceLanguage::of(&path);
        let prepared = prepare(&source, families, language);
        for (family, key) in sites
            .keys()
            .filter_map(|family| {
                family
                    .strip_prefix(&prefix)
                    .map(|key| (family.clone(), key.to_owned()))
            })
            .collect::<Vec<_>>()
        {
            let (family, key) = (family, key.as_str());
            let found = sites.get_mut(&family).expect("seeded family");
            // `families_read` has already decided this file reads the family
            // (through the path, a loader, or a domain string), so the count is
            // added whichever way it was reached. A file with no `families::`
            // path still counts: a loader call is a replay, and the per-key
            // count is zero only when nothing spells it out.
            // Quoted spellings count too: a `.ts` conformance table names a
            // reader as a string literal, "families::<key>::cases()". The
            // language decides which text `prepare` counted in, so nothing is
            // added back here.
            let calls = reader_count_prepared(&prepared, key)
                .unwrap_or_else(|error| panic!("{relative}: {error}"));
            // A zero spelling count does not mean no reader: `families_read`
            // above may have recorded this file already, through a loader.
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

    /// The families these tests scan for: one derived key, plus siblings so an
    /// alias cannot leak between families.
    fn scan_families() -> BTreeSet<String> {
        [
            "families/alpha_family",
            "families/beta_family",
            "families/gamma_family",
        ]
        .iter()
        .map(|family| (*family).to_owned())
        .collect()
    }

    fn count(source: &str, key: &str) -> usize {
        let families = scan_families();
        reader_count(source, key, &families).expect("no ambiguous alias")
    }

    fn ts_count(source: &str, key: &str) -> usize {
        let families = scan_families();
        reader_count_in(source, key, &families, SourceLanguage::TypeScript)
            .expect("no ambiguous alias")
    }

    /// An alias that is replayed or enumerated is a reader of that family.
    #[test]
    fn an_aliased_family_counts_as_a_reader() {
        for alias in ["wire", "scenario"] {
            for call in ["::replay(", "::cases("] {
                let source = format!(
                    "use gents_lean_vocab::families::alpha_family as {alias};\n\
                     fn t() {{ {alias}{call}assert!(true) }}"
                );
                assert_eq!(
                    count(&source, "alpha_family"),
                    1,
                    "`as {alias};` + `{alias}{call}` is one reader: {source}"
                );
            }
        }
    }

    /// A declaration that never calls replay or cases reads nothing: binding a
    /// name is not reading a family.
    #[test]
    fn an_alias_without_a_call_is_not_a_reader() {
        let source = "use gents_lean_vocab::families::alpha_family as wire;\n\
                      fn t() { let _ = wire::RefusalInput::default(); }";
        assert_eq!(count(source, "alpha_family"), 0);
    }

    /// A bare `replay(` with no family path is nobody's reader. The alias must
    /// be the receiver, which is what separates this from widening the scan.
    #[test]
    fn a_bare_replay_call_is_not_a_reader() {
        for source in [
            "fn t() { replay(|| handle(input)); }",
            "fn t() { cases(|c| c.value()); }",
            "use gents_lean_vocab::families::alpha_family as wire;\n\
             fn t() { other::replay(|| handle(input)); }",
        ] {
            assert_eq!(count(source, "alpha_family"), 0, "{source}");
        }
    }

    /// An alias belongs to the family it was declared for: the same name bound
    /// to `beta_family` must not read as a reader of `alpha_family`.
    #[test]
    fn an_alias_only_reads_the_family_it_is_declared_for() {
        let source = "use gents_lean_vocab::families::beta_family as wire;\n\
                      fn t() { wire::replay(|_| Ok(())); }";
        assert_eq!(count(source, "beta_family"), 1);
        assert_eq!(
            count(source, "alpha_family"),
            0,
            "beta's alias is not alpha's"
        );
    }

    /// Counting is per occurrence: one file holds many tests, so two replays in
    /// one file read as two and the "exactly one replay" rule can see them.
    #[test]
    fn each_replay_counts_once_however_many_are_in_the_file() {
        let canonical =
            "fn a() { gents_lean_vocab::families::alpha_family::replay(|_| Ok(())); }\n\
                         fn b() { gents_lean_vocab::families::alpha_family::replay(|_| Ok(())); }";
        assert_eq!(count(canonical, "alpha_family"), 2, "two canonical replays");

        let aliased = "use gents_lean_vocab::families::alpha_family as wire;\n\
                       fn a() { wire::replay(|_| Ok(())); }\n\
                       fn b() { wire::replay(|_| Ok(())); }\n\
                       fn c() { wire::cases(); }";
        assert_eq!(count(aliased, "alpha_family"), 3, "three aliased uses");

        // A canonical call and an aliased use are two readers, in one file.
        let mixed = "use gents_lean_vocab::families::alpha_family as wire;\n\
                     fn a() { gents_lean_vocab::families::alpha_family::cases(); }\n\
                     fn b() { wire::replay(|_| Ok(())); }";
        assert_eq!(count(mixed, "alpha_family"), 2);
    }

    /// The `as family;` declaration keeps its pre-existing behaviour: one per
    /// declaration, and its `family::replay(` uses are not counted again.
    #[test]
    fn the_conventional_alias_counts_per_declaration_only() {
        let source = "use gents_lean_vocab::families::alpha_family as family;\n\
                      fn t() { family::replay(|_| Ok(())); }";
        assert_eq!(
            count(source, "alpha_family"),
            1,
            "declaration counts, uses do not"
        );
    }

    /// The alias must stand on its own. `rewire::replay(` and
    /// `crate::wire::replay(` are not uses of `wire`.
    #[test]
    fn an_alias_must_sit_on_a_word_boundary() {
        let declare = "use gents_lean_vocab::families::alpha_family as wire;\n";
        for call in [
            "rewire::replay(|_| Ok(()))",
            "crate::wire::replay(|_| Ok(()))",
            "other_wire::replay(|_| Ok(()))",
            "super::wire::replay(|_| Ok(()))",
        ] {
            let source = format!("{declare}fn t() {{ {call}; }}");
            assert_eq!(
                count(&source, "alpha_family"),
                0,
                "`{call}` is not a use of `wire`: {source}"
            );
        }
        // And the plain form does count.
        let source = format!("{declare}fn t() {{ wire::replay(|_| Ok(())); }}");
        assert_eq!(count(&source, "alpha_family"), 1);
    }

    /// A commented-out or quoted declaration is not a declaration.
    #[test]
    fn comments_and_strings_are_not_read() {
        for source in [
            "// use gents_lean_vocab::families::alpha_family as wire;\n\
             fn t() { wire::replay(|_| Ok(())); }",
            "/* use gents_lean_vocab::families::alpha_family as wire; */\n\
             fn t() { wire::replay(|_| Ok(())); }",
            "/// use gents_lean_vocab::families::alpha_family as wire;\n\
             fn t() { wire::replay(|_| Ok(())); }",
            "fn t() { let s = \"use gents_lean_vocab::families::alpha_family as wire;\"; \
             wire::replay(|_| Ok(())); }",
        ] {
            assert_eq!(count(source, "alpha_family"), 0, "{source}");
        }
        // The same declaration outside a comment does read.
        let live = "use gents_lean_vocab::families::alpha_family as wire;\n\
                    fn t() { wire::replay(|_| Ok(())); }";
        assert_eq!(count(live, "alpha_family"), 1);
    }

    /// Two families sharing one alias in one file cannot be attributed, so
    /// neither is credited and the scan says so loudly rather than letting one
    /// family ride on the other's replay.
    #[test]
    fn one_alias_for_two_families_is_ambiguous_and_counted_for_neither() {
        let families = scan_families();
        let source = "use gents_lean_vocab::families::alpha_family as wire;\n\
                      use gents_lean_vocab::families::beta_family as wire;\n\
                      fn t() { wire::replay(|_| Ok(())); }";
        for key in ["alpha_family", "beta_family"] {
            let error = reader_count(source, key, &families)
                .expect_err("an alias naming two families is ambiguous");
            assert!(
                error.contains("ambiguous alias") && error.contains("wire"),
                "the message names the alias: {error}"
            );
        }
    }

    /// The failure message lists the spellings it accepts, and says which forms
    /// it does not recognise, so the next reader is not misled as B8 was.
    #[test]
    fn the_failure_message_names_the_accepted_spellings() {
        let message = accepted_spellings();
        for expected in [
            "families::<key>::replay(",
            "families::<key>::cases(",
            "families::<key> as family;",
            "as <alias>;",
            "<alias>::replay(",
            "one per occurrence",
            "grouped import",
            "alias equal to a family key",
            "bare `replay(`",
            "word boundary",
            "comment or a string literal",
        ] {
            assert!(
                message.contains(expected),
                "message omits {expected}: {message}"
            );
        }
        // The *prose* balances: the old text opened a parenthesis and closed it
        // two lines later. The spelling lines carry their own `(`/`)` in
        // `::replay(` and `::cases(`, so they are excluded from the count.
        for line in message
            .lines()
            // Spelling lines and inline code carry their own `(`/`)`.
            .filter(|line| {
                !line.contains("families::") && !line.contains("<alias>::") && !line.contains('`')
            })
        {
            assert_eq!(
                line.matches('(').count(),
                line.matches(')').count(),
                "prose parentheses balance: {line}"
            );
        }
    }

    /// **Blocker.** A `.ts` conformance table names its reader in a string, so the
    /// quoted spelling counts for a TypeScript file.
    #[test]
    fn a_typescript_table_string_counts_as_a_reader() {
        let source = "export const CASES = [\n\
                      \x20 ('families::alpha_family::cases()'),\n\
                      \x20 ('families::alpha_family::replay(input)'),\n\
                      ];\n";
        assert_eq!(
            ts_count(source, "alpha_family"),
            2,
            "two quoted spellings are two readers"
        );
    }

    /// A commented-out spelling in a TypeScript file is not a reader.
    #[test]
    fn a_typescript_comment_does_not_count() {
        for source in [
            "// ('families::alpha_family::cases()')\nexport const X = 1;\n",
            "/* ('families::alpha_family::cases()') */\nexport const X = 1;\n",
        ] {
            assert_eq!(ts_count(source, "alpha_family"), 0, "a comment: {source}");
        }
    }

    /// **Blocker.** A `.rs` string restores nothing: a quoted spelling there is a
    /// panic message or a fixture source, not a reader.
    #[test]
    fn a_rust_string_does_not_count() {
        let source = "fn t() { panic!(\"families::alpha_family::cases()\"); }\n\
                      const FIXTURE: &str = \"families::alpha_family::replay(x)\";\n";
        assert_eq!(
            count(source, "alpha_family"),
            0,
            "a Rust string is not a reader"
        );
    }

    /// And a commented-out spelling in a Rust file is not a reader either.
    #[test]
    fn a_rust_comment_does_not_count() {
        for source in [
            "// families::alpha_family::cases()\nfn t() {}\n",
            "/* families::alpha_family::cases() */\nfn t() {}\n",
            "/// families::alpha_family::replay(x)\nfn t() {}\n",
        ] {
            assert_eq!(count(source, "alpha_family"), 0, "{source}");
        }
    }

    /// A raw string needs `r`, zero or more `#`, then `"`. `r#type` and `r#try`
    /// are identifiers, and the hash count must come from the `r`, not the token
    /// start: `br#"…"#` once looked for `"##` and blanked the rest of the file.
    #[test]
    fn a_raw_hash_identifier_is_not_a_string() {
        let source = "fn t(r#type: &str) { gents_lean_vocab::families::alpha_family::cases(); }\n\
                      fn u(r#try: u8) {}\n";
        assert_eq!(
            count(source, "alpha_family"),
            1,
            "a raw identifier is not a string"
        );

        let raw = "const S: &str = r#\"families::alpha_family::cases()\"#;\nfn t() {}\n";
        assert_eq!(count(raw, "alpha_family"), 0, "a raw string is blanked");

        // A byte raw string is blanked, and a reader after it survives: the hash
        // count is one, so the closer is `"#`, not `"##`.
        let byte_raw = "let p = format!(br#\"{}\"#, 1);\n\
                        fn t() { gents_lean_vocab::families::alpha_family::cases(); }\n";
        assert_eq!(
            count(byte_raw, "alpha_family"),
            1,
            "br#\"…\"# closes at `\"#`, so the reader after it counts"
        );
    }

    /// Lifetimes are not char literals: `&'a T, &'b U` must not blank the body,
    /// and neither must an apostrophe in prose.
    #[test]
    fn lifetimes_are_not_char_literals() {
        let source = "fn t<'a, 'b>(x: &'a T, y: &'b U) {\n\
                      \x20 gents_lean_vocab::families::alpha_family::cases();\n\
                      }\n";
        assert_eq!(count(source, "alpha_family"), 1, "lifetimes blank nothing");

        let prose = "fn t() {\n\
                     \x20 let m = format!(\"the user's build\");\n\
                     \x20 gents_lean_vocab::families::alpha_family::cases();\n\
                     }\n";
        assert_eq!(
            count(prose, "alpha_family"),
            1,
            "an apostrophe in a string must not swallow the reader after it"
        );

        let ch =
            "const C: char = 'x';\nfn t() { gents_lean_vocab::families::alpha_family::cases(); }\n";
        assert_eq!(count(ch, "alpha_family"), 1, "a char literal is blanked");
    }

    /// A TypeScript template literal spans lines. It is a string, and a `.ts`
    /// spelling is counted with strings kept, so its contents count like any
    /// other string's — and, crucially, the template terminates rather than
    /// swallowing the file.
    #[test]
    fn a_typescript_template_literal_spans_lines_and_terminates() {
        // The decisive shape: the spelling is **on** a `//`-led line inside a
        // multi-line template. If the template stopped at its first newline, that
        // line would be read as a comment and the spelling blanked — the exact
        // undercount this test exists to catch. (A spelling on a later, unled
        // line would survive either way, so it would not test this.)
        let source = "const q = `\n\
                      // families::alpha_family::cases()\n\
                      `;\n";
        assert_eq!(
            ts_count(source, "alpha_family"),
            1,
            "a `//`-led line inside a multi-line template is text, not a comment: {source}"
        );

        // And a single-line template still counts its contents.
        let inline = "export const X = ('families::alpha_family::replay(i)');\n";
        assert_eq!(ts_count(inline, "alpha_family"), 1);

        // A comment *after* a template is still blanked, so the template
        // terminated rather than swallowing the rest of the file.
        let after_comment = "const q = `x`;\n\
                             // families::alpha_family::cases()\n\
                             export const X = ('families::alpha_family::replay(i)');\n";
        assert_eq!(
            ts_count(after_comment, "alpha_family"),
            1,
            "a comment after a template is still a comment: {after_comment}"
        );
    }

    /// An unmatched quote stops at its line rather than swallowing the file.
    #[test]
    fn an_unmatched_quote_stops_at_the_end_of_its_line() {
        let source = "const broken = 'no closing quote\n\
                      export const X = ('families::alpha_family::cases()');\n";
        assert_eq!(
            ts_count(source, "alpha_family"),
            1,
            "a stray quote must not swallow the readers after it: {source}"
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
