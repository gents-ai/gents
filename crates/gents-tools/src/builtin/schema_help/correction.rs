use serde_json::{Map, Value};

use super::{preview_call, SchemaParams};

const KNOWN_RESOURCES: &[&str] = &[
    "collection",
    "version",
    "view",
    "migration",
    "batch",
    "help",
];

const GET_SYNONYMS: &[&str] = &["describe", "inspect", "show", "info", "fields", "schema"];

pub(super) fn argument_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    explain_correction(args)
        .or_else(|| plural_resource_correction(args))
        .or_else(|| verb_first_correction(args))
        .or_else(|| collection_verb_alias_correction(args))
        .or_else(|| collection_first_correction(args))
        .or_else(|| canonical_command_correction(args))
}

fn collection_get_params(name: &str) -> SchemaParams {
    SchemaParams {
        argv: vec!["collection".into(), "get".into(), name.to_owned()],
        target_id: None,
        options: Map::new(),
    }
}

fn collection_list_params(options: &Map<String, Value>) -> SchemaParams {
    let mut kept = Map::new();
    if let Some(scope) = options.get("scope") {
        kept.insert("scope".into(), scope.clone());
    }
    SchemaParams {
        argv: vec!["collection".into(), "list".into()],
        target_id: None,
        options: kept,
    }
}

fn help_params(topic: &[&str]) -> SchemaParams {
    let mut argv = vec!["help".to_owned()];
    argv.extend(topic.iter().map(|word| (*word).to_owned()));
    SchemaParams {
        argv,
        target_id: None,
        options: Map::new(),
    }
}

fn resolve_single_target<'a>(
    positional: Option<&'a str>,
    target_id: Option<&'a str>,
) -> Option<&'a str> {
    match (positional, target_id.filter(|id| !id.is_empty())) {
        (Some(pos), Some(target)) if pos == target => Some(pos),
        (Some(_), Some(_)) => None,
        (Some(pos), None) => Some(pos),
        (None, Some(target)) => Some(target),
        (None, None) => None,
    }
}

fn strip_preview<'a>(words: &'a [&'a str]) -> Vec<&'a str> {
    words
        .iter()
        .copied()
        .filter(|word| *word != "preview")
        .collect()
}

fn explain_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let words: Vec<&str> = args.argv.iter().map(String::as_str).collect();
    let name = match words.as_slice() {
        ["explain"] => args.target_id.as_deref().filter(|id| !id.is_empty()),
        ["explain", name] => {
            if args.target_id.as_deref().is_some_and(|id| id != *name) {
                return None;
            }
            Some(*name)
        }
        [first, "explain"]
            if !KNOWN_RESOURCES.contains(first)
                && !matches!(*first, "collections" | "versions" | "views" | "migrations") =>
        {
            if args.target_id.as_deref().is_some_and(|id| id != *first) {
                return None;
            }
            Some(*first)
        }
        ["collection" | "collections", "explain"] => {
            args.target_id.as_deref().filter(|id| !id.is_empty())
        }
        ["collection" | "collections", "explain", name] => {
            if args.target_id.as_deref().is_some_and(|id| id != *name) {
                return None;
            }
            Some(*name)
        }
        _ => return None,
    };
    if let Some(name) = name {
        Some((
            format!(
                "schema has no \"explain\" command; inspect {name:?}'s schema and indexes with [\"collection\", \"get\", {name:?}], or inspect a query plan with the query tool ({{\"argv\":[\"explain\"],\"collection\":{name:?},\"options\":{{\"fields\":[...]}}}})"
            ),
            collection_get_params(name),
        ))
    } else {
        Some((
            "schema has no \"explain\" command; list collections with [\"collection\", \"list\"], inspect a schema with [\"collection\", \"get\", NAME], or inspect a query plan with the query tool (argv:[\"explain\"])".into(),
            collection_list_params(&Map::new()),
        ))
    }
}

fn singular_resource(word: &str) -> Option<&'static str> {
    match word {
        "collections" => Some("collection"),
        "versions" => Some("version"),
        "views" => Some("view"),
        "migrations" => Some("migration"),
        _ => None,
    }
}

fn plural_resource_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let first = args.argv.first().map(String::as_str)?;
    let singular = singular_resource(first)?;
    let words: Vec<&str> = args.argv.iter().map(String::as_str).collect();
    if words.len() == 1 {
        let next = match singular {
            "collection" => collection_list_params(&args.options),
            "version" => SchemaParams {
                argv: vec!["version".into(), "list".into()],
                target_id: args.target_id.clone(),
                options: Map::new(),
            },
            _ => help_params(&[singular]),
        };
        let message = format!(
            "schema uses singular resource {singular:?}, not {first:?}; use {:?} or read [\"help\", {singular:?}]",
            next.argv
        );
        return Some((message, next));
    }
    if singular == "version"
        && words.len() == 2
        && !matches!(words[1], "list" | "get" | "activate" | "preview")
    {
        return Some((
            format!(
                "schema uses singular resource \"version\", not \"versions\"; use [\"version\", \"list\", {:?}]",
                words[1]
            ),
            SchemaParams {
                argv: vec!["version".into(), "list".into(), words[1].to_owned()],
                target_id: None,
                options: Map::new(),
            },
        ));
    }
    let mut rewritten = args.clone();
    rewritten.argv[0] = singular.to_owned();
    let stripped_owned: Vec<String> = rewritten
        .argv
        .iter()
        .filter(|word| word.as_str() != "preview")
        .cloned()
        .collect();
    let stripped: Vec<&str> = stripped_owned.iter().map(String::as_str).collect();
    let valid_read = matches!(
        stripped.as_slice(),
        ["collection", "list"]
            | ["collection", "get"]
            | ["collection", "get", _]
            | ["version", "list"]
            | ["version", "list", _]
            | ["version", "get"]
            | ["version", "get", _]
    );
    if valid_read {
        rewritten.argv = stripped_owned;
        rewritten.options.remove("digest");
        return Some((
            format!(
                "schema uses singular resource {singular:?}, not {first:?}; use {:?}",
                rewritten.argv
            ),
            rewritten,
        ));
    }
    if let Some((_, corrected)) = collection_verb_alias_correction(&rewritten)
        .or_else(|| canonical_command_correction(&rewritten))
    {
        return Some((
            format!(
                "schema uses singular resource {singular:?}, not {first:?}; review the corrected call (it has not been executed)"
            ),
            corrected,
        ));
    }
    if let Some(next) = singular_mutation_next(&rewritten, &stripped) {
        return Some((
            format!(
                "schema uses singular resource {singular:?}, not {first:?}; review the corrected call (it has not been executed)"
            ),
            next,
        ));
    }
    Some((
        format!(
            "schema uses singular resource {singular:?}, not {first:?}; read [\"help\", {singular:?}] for valid verbs"
        ),
        help_params(&[singular]),
    ))
}

fn singular_mutation_next(rewritten: &SchemaParams, stripped: &[&str]) -> Option<SchemaParams> {
    let mut next = rewritten.clone();
    next.argv = stripped.iter().map(|word| (*word).to_owned()).collect();
    match stripped {
        ["collection", "create"] => {
            if !next.options.contains_key("sdl") && !next.options.contains_key("schema") {
                return Some(help_params(&["collection", "create"]));
            }
            if !next.options.contains_key("sdl") {
                if let Some(sdl) = next.options.remove("schema") {
                    next.options.insert("sdl".into(), sdl);
                }
            }
            next.target_id = None;
            Some(preview_call(&next))
        }
        ["collection", "update"] | ["collection", "update", _] => {
            if !next.options.contains_key("patch") {
                return Some(help_params(&["collection", "update"]));
            }
            Some(preview_call(&next))
        }
        ["collection", "materialize" | "empty" | "remove"]
        | ["collection", "materialize" | "empty" | "remove", _]
        | ["version", "activate"]
        | ["version", "activate", _] => {
            if stripped.len() < 3 && next.target_id.is_none() {
                return Some(help_params(&[stripped[0], stripped[1]]));
            }
            Some(preview_call(&next))
        }
        ["view", "create"] => {
            if !next.options.contains_key("query") || !next.options.contains_key("sdl") {
                return Some(help_params(&["view", "create"]));
            }
            next.target_id = None;
            Some(preview_call(&next))
        }
        ["migration", "set"] => {
            if !next.options.contains_key("config") {
                return Some(help_params(&["migration", "set"]));
            }
            next.target_id = None;
            Some(preview_call(&next))
        }
        _ => None,
    }
}

fn verb_first_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let words: Vec<&str> = args.argv.iter().map(String::as_str).collect();
    let stripped = strip_preview(&words);
    match stripped.as_slice() {
        ["list" | "ls"] | ["list" | "ls", "collection" | "collections"] => Some((
            format!(
                "start argv with resource \"collection\", not {:?}; use [\"collection\", \"list\"] to list collections",
                words[0]
            ),
            collection_list_params(&args.options),
        )),
        [verb] if *verb == "get" || GET_SYNONYMS.contains(verb) => {
            if let Some(name) = args.target_id.as_deref().filter(|id| !id.is_empty()) {
                Some((
                    format!(
                        "start argv with resource \"collection\", not {verb:?}; use [\"collection\", \"get\", {name:?}] to inspect its schema"
                    ),
                    collection_get_params(name),
                ))
            } else {
                Some((
                    format!(
                        "start argv with resource \"collection\", not {verb:?}; use [\"collection\", \"list\"] to discover names, then [\"collection\", \"get\", NAME]"
                    ),
                    collection_list_params(&Map::new()),
                ))
            }
        }
        [verb, name] if *verb == "get" || GET_SYNONYMS.contains(verb) => {
            let target = resolve_single_target(Some(name), args.target_id.as_deref())?;
            Some((
                format!(
                    "start argv with resource \"collection\", not {verb:?}; use [\"collection\", \"get\", {target:?}] to inspect its schema"
                ),
                collection_get_params(target),
            ))
        }
        ["create"] => {
            let resource = if args.options.contains_key("query") {
                "view"
            } else {
                "collection"
            };
            let mut next = args.clone();
            next.argv = vec![resource.into(), "create".into()];
            next.target_id = None;
            if !next.options.contains_key("sdl") && next.options.contains_key("schema") {
                if let Some(sdl) = next.options.remove("schema") {
                    next.options.insert("sdl".into(), sdl);
                }
            }
            let next = if next.options.contains_key("sdl")
                && (resource != "view" || next.options.contains_key("query"))
            {
                preview_call(&next)
            } else {
                help_params(&[resource, "create"])
            };
            Some((
                format!(
                    "start argv with resource {resource:?}, not \"create\"; use [\"{resource}\", \"preview\", \"create\"] with options.sdl"
                ),
                next,
            ))
        }
        [verb] | [verb, _] => {
            let canonical = canonical_collection_mutation_verb(verb)?;
            let target = resolve_single_target(stripped.get(1).copied(), args.target_id.as_deref());
            let next = build_collection_mutation_next(canonical, target, &args.options);
            Some((
                format!(
                    "start argv with resource \"collection\", not {verb:?}; use [\"collection\", \"preview\", {canonical:?}, NAME]"
                ),
                next,
            ))
        }
        _ => None,
    }
}

fn canonical_collection_mutation_verb(verb: &str) -> Option<&'static str> {
    match verb {
        "update" | "patch" | "alter" | "edit" | "evolve" => Some("update"),
        "materialize" => Some("materialize"),
        "empty" | "truncate" | "clear" => Some("empty"),
        "remove" | "delete" | "drop" => Some("remove"),
        _ => None,
    }
}

fn build_collection_mutation_next(
    canonical: &str,
    target: Option<&str>,
    options: &Map<String, Value>,
) -> SchemaParams {
    let Some(name) = target else {
        return help_params(&["collection", canonical]);
    };
    if canonical == "update" && !options.contains_key("patch") {
        return help_params(&["collection", "update"]);
    }
    let mut kept = Map::new();
    if canonical == "update" {
        if let Some(patch) = options.get("patch") {
            kept.insert("patch".into(), patch.clone());
        }
    }
    SchemaParams {
        argv: vec![
            "collection".into(),
            "preview".into(),
            canonical.into(),
            name.to_owned(),
        ],
        target_id: None,
        options: kept,
    }
}

fn collection_verb_alias_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let words: Vec<&str> = args.argv.iter().map(String::as_str).collect();
    let stripped = strip_preview(&words);
    if stripped.first().copied() != Some("collection") {
        return None;
    }
    let verb = *stripped.get(1)?;
    if GET_SYNONYMS.contains(&verb) {
        if let Some(name) =
            resolve_single_target(stripped.get(2).copied(), args.target_id.as_deref())
        {
            return Some((
                format!(
                    "collection has no {verb:?} verb; use [\"collection\", \"get\", {name:?}] to inspect its schema"
                ),
                collection_get_params(name),
            ));
        }
        return Some((
            format!(
                "collection has no {verb:?} verb; use [\"collection\", \"list\"] to discover names, then [\"collection\", \"get\", NAME]"
            ),
            collection_list_params(&Map::new()),
        ));
    }
    if verb == "add" {
        let mut next = args.clone();
        next.argv = vec!["collection".into(), "create".into()];
        next.target_id = None;
        if !next.options.contains_key("sdl") && next.options.contains_key("schema") {
            if let Some(sdl) = next.options.remove("schema") {
                next.options.insert("sdl".into(), sdl);
            }
        }
        let next = if next.options.contains_key("sdl") {
            preview_call(&next)
        } else {
            help_params(&["collection", "create"])
        };
        return Some((
            "collection has no \"add\" verb; use [\"collection\", \"preview\", \"create\"] with options.sdl".into(),
            next,
        ));
    }
    if matches!(
        verb,
        "patch" | "alter" | "edit" | "evolve" | "delete" | "drop" | "truncate" | "clear"
    ) {
        let canonical = canonical_collection_mutation_verb(verb)?;
        let target = resolve_single_target(stripped.get(2).copied(), args.target_id.as_deref());
        let next = build_collection_mutation_next(canonical, target, &args.options);
        return Some((
            format!(
                "collection has no {verb:?} verb; use [\"collection\", \"preview\", {canonical:?}, NAME]"
            ),
            next,
        ));
    }
    None
}

fn collection_first_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let words: Vec<&str> = args.argv.iter().map(String::as_str).collect();
    let first = *words.first()?;
    if KNOWN_RESOURCES.contains(&first) || singular_resource(first).is_some() {
        return None;
    }
    if words.len() == 1 {
        if first.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && gents_store::graphql::validate_collection_identifier(first).is_ok()
        {
            return Some((
                format!(
                    "unknown schema resource {first:?}; start argv with resource \"collection\", not a collection name. Use [\"collection\", \"get\", {first:?}] to inspect its schema, or [\"collection\", \"list\"] to list collections"
                ),
                collection_get_params(first),
            ));
        }
        return None;
    }
    let rest = strip_preview(&words[1..]);
    if args.target_id.as_deref().is_some_and(|id| id != first) {
        return None;
    }
    match rest.as_slice() {
        [verb] if *verb == "get" || GET_SYNONYMS.contains(verb) => Some((
            format!(
                "unknown schema resource {first:?}; start argv with resource \"collection\", not a collection name. Use [\"collection\", \"get\", {first:?}] to inspect its schema"
            ),
            collection_get_params(first),
        )),
        ["version" | "versions"] | ["version" | "versions", "list"] => Some((
            format!(
                "unknown schema resource {first:?}; start argv with resource \"version\", not a collection name. Use [\"version\", \"list\", {first:?}]"
            ),
            SchemaParams {
                argv: vec!["version".into(), "list".into(), first.to_owned()],
                target_id: None,
                options: Map::new(),
            },
        )),
        [verb] => {
            let canonical = canonical_collection_mutation_verb(verb)?;
            let next = build_collection_mutation_next(canonical, Some(first), &args.options);
            Some((
                format!(
                    "unknown schema resource {first:?}; start argv with resource \"collection\", not a collection name. Use [\"collection\", \"preview\", {canonical:?}, {first:?}]"
                ),
                next,
            ))
        }
        _ => None,
    }
}

fn canonical_command_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let mut next = args.clone();
    if next.argv.get(1).is_some_and(|word| word == "preview") {
        next.argv.remove(1);
    }
    let mut reasons = Vec::new();
    let create = matches!(
        next.argv.first().map(String::as_str),
        Some("collection" | "view")
    ) && next.argv.get(1).is_some_and(|word| word == "create");
    if create && (next.target_id.is_some() || next.argv.len() == 3) {
        next.target_id = None;
        next.argv.truncate(2);
        reasons.push(
            "create takes collection names from options.sdl; omit target_id and positional names",
        );
    }
    if create && !next.options.contains_key("sdl") && next.options.contains_key("schema") {
        let sdl = next.options.remove("schema").unwrap();
        next.options.insert("sdl".into(), sdl);
        reasons.push("SDL belongs in options.sdl, not options.schema");
    }
    let read = matches!(
        next.argv
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["collection" | "version", "list" | "get"] | ["collection" | "version", "list" | "get", _]
    );
    if read
        && (args.argv.get(1).is_some_and(|word| word == "preview")
            || next.options.contains_key("digest"))
    {
        next.options.remove("digest");
        reasons.push("reads need neither preview nor digest");
    }
    if reasons.is_empty() {
        return None;
    }
    if !read {
        next = preview_call(&next);
    }
    Some((
        format!(
            "{}. Review the corrected call; it has not been executed",
            reasons.join("; ")
        ),
        next,
    ))
}
