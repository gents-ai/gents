use anyhow::{bail, Result};

mod collections;
mod concepts;
mod indexes;
mod lenses;

pub(super) const PATHS: &[&str] = &[
    "collections",
    "collections policy",
    "collections branchable",
    "collections governed",
    "collections downsample",
    "views",
    "views virtual",
    "views materialized",
    "views refresh",
    "fields",
    "fields types",
    "fields nullability",
    "fields arrays",
    "fields defaults",
    "fields crdt",
    "fields immutable",
    "fields constraints",
    "relationships",
    "relationships one-to-many",
    "relationships one-to-one",
    "relationships named",
    "relationships self",
    "evolution",
    "evolution additive",
    "evolution versions",
    "evolution migrations",
    "indexes",
    "indexes ordered",
    "indexes unique",
    "indexes composite",
    "indexes vector",
    "indexes vector dimensions",
    "indexes vector metrics",
    "indexes vector hnsw",
    "indexes vector flat",
    "indexes vector ivfpq",
    "indexes vector ivfflat",
    "indexes vector ssg",
    "indexes fulltext",
    "indexes encrypted",
    "embeddings",
    "embeddings generation",
    "embeddings indexing",
    "migration workflow",
    "migration authoring",
    "migration authoring rust",
    "migration build",
    "migration contract",
    "migration contract memory",
    "migration contract documents",
    "migration arguments",
    "migration inverse",
    "migration versions",
    "migration verify",
    "migration verify host",
    "migration recovery",
    "batch",
    "collection",
    "collection list",
    "collection get",
    "collection create",
    "collection update",
    "collection materialize",
    "version",
    "version list",
    "version get",
    "version activate",
    "migration",
    "migration set",
    "view",
    "view create",
];

fn equivalent(left: &str, right: &str) -> bool {
    fn normalize(word: &str) -> &str {
        match word {
            "collection" | "collections" => "collection",
            "view" | "views" => "view",
            "index" | "indexes" => "index",
            "field" | "fields" => "field",
            "relationship" | "relationships" => "relationship",
            "embedding" | "embeddings" => "embedding",
            "version" | "versions" => "version",
            "migration" | "migrations" => "migration",
            other => other,
        }
    }
    normalize(left) == normalize(right)
}

pub(super) fn resolve(path: &[String]) -> Option<Vec<String>> {
    if !path.is_empty()
        && path
            .iter()
            .all(|word| matches!(word.to_ascii_lowercase().as_str(), "concepts" | "sdl"))
    {
        return Some(Vec::new());
    }
    if lookup(path).is_ok() {
        return Some(path.to_vec());
    }
    for start in 0..path.len() {
        let requested = &path[start..];
        let matches: Vec<Vec<String>> = PATHS
            .iter()
            .filter_map(|candidate| {
                let words: Vec<_> = candidate.split_whitespace().map(str::to_owned).collect();
                (words.len() >= requested.len()
                    && words[words.len() - requested.len()..]
                        .iter()
                        .zip(requested)
                        .all(|(actual, requested)| equivalent(actual, requested)))
                .then_some(words)
            })
            .collect();
        if matches.len() == 1 {
            return matches.into_iter().next();
        }
        if start == 0 && matches.len() > 1 {
            break;
        }
    }
    None
}

pub(super) fn recovery_path(path: &[String]) -> Vec<String> {
    if let Some(path) = resolve(path) {
        return path;
    }
    for depth in (1..path.len()).rev() {
        if let Some(parent) = resolve(&path[..depth]) {
            return parent;
        }
    }
    Vec::new()
}

pub(super) fn page(path: &[String]) -> Result<&'static str> {
    if let Some(canonical) = resolve(path) {
        return lookup(&canonical);
    }
    let mut next = vec!["help".to_owned()];
    next.extend(recovery_path(path));
    bail!(
        "unknown or ambiguous schema help topic; read available topics with argv:{}",
        serde_json::to_string(&next)?
    )
}

fn lookup(path: &[String]) -> Result<&'static str> {
    let words: Vec<_> = path.iter().map(String::as_str).collect();
    if let Some(page) = concepts::page(&words)
        .or_else(|| indexes::page(&words))
        .or_else(|| collections::page(&words))
        .or_else(|| lenses::page(&words))
    {
        return Ok(page);
    }
    match words.as_slice() {
        [] => Ok("schema RESOURCE VERB; inputs go in options. Names for collection/view create come from options.sdl: omit IDs. Other commands that take an ID accept it after the verb or in target_id.\ncollection: list, get, create, update, materialize\nversion: list, get, activate\nmigration: set\nview: create\nbatch: ordered calls in options.operations\nSDL concepts: help collections | fields | relationships | indexes | embeddings | evolution | views. Add subtopics to narrow the help, e.g. help indexes vector hnsw. Each page lists its children.\nCommand inputs: RESOURCE VERB --help.\nMutations require RESOURCE preview VERB first; inspect the effect, then use the returned next_call with options.digest. Changed inputs or schemas require a new preview. Reads need neither. Gents-managed schemas evolve through product releases. This tool changes definitions, not document permissions."),
        ["batch"] => Ok("batch takes options.operations: 1–64 schema calls. Calls run in order; the first error stops the batch, earlier changes remain, and later calls are not attempted. Nested batches are rejected. Each mutation needs its own preview result. Preview dependent operations after their dependencies commit."),
        ["collection"] => Ok("collection defines stored document fields. list and get inspect definitions; create installs new definitions; update patches existing ones; materialize advances cached documents through registered migrations. Use collection VERB --help for inputs. SDL: help collections, help fields, help relationships, help indexes, help embeddings. Changes to existing definitions: help evolution. Collection deletion is not exposed by this tool."),
        ["collection", "list"] => Ok("collection list: no ID or options. Returns collection names; inspect one with collection get NAME."),
        ["collection", "get"] => Ok("collection get NAME: returns DefraDB field types, version and metadata. NAME may instead use target_id. No options."),
        ["collection", "create"] => Ok(r#"collection preview create: options.sdl is GraphQL SDL; collection names come from it, so omit target_id. Example: {"argv":["collection","preview","create"],"options":{"sdl":"type WorkItem { handoff_id: String, title: String }"}}. Inspect the effect, then use next_call to create with options.digest. Scalars include String, Int, Float, Boolean and DateTime. An existing matching definition is a no-op; use collection update to change one."#),
        ["collection", "update"] => Ok(r#"collection preview update NAME: options.patch is an RFC 6902 array using DefraDB's field names and /COLLECTION/... paths. Example: {"argv":["collection","preview","update","WorkItem"],"options":{"patch":[{"op":"add","path":"/WorkItem/Fields/-","value":{"Name":"handoff_id","Kind":"String"}}]}}. For scalar Kind names, use SDL spelling: Boolean, not Bool. Read help fields types. Inspect get before using field indexes, then review the preview and use next_call. DefraDB validates changes and may create a new version. A nullable field addition needs no lens; old rows have no supplied value. Update writers separately through config; never fabricate historical values."#),
        ["collection", "materialize"] => Ok("collection preview materialize NAME: no options. Review the preview and use next_call to advance cached documents through registered migrations to the active version. This creates no document commits and synthesizes no business values."),
        ["version"] => Ok("version inspects published collection definitions. list discovers exact VersionIDs; get inspects one; activate makes one active. Use version VERB --help for inputs."),
        ["version", "list"] => Ok("version list [NAME]: optionally filter by collection name, either after list or in target_id. Includes active and inactive definitions and exact VersionIDs. No options."),
        ["version", "get"] => Ok("version get VERSION_ID: copy an exact VersionID from version list. It may instead use target_id. Returns definition and transform metadata. No options."),
        ["version", "activate"] => Ok("version preview activate VERSION_ID: no options. Review the effect, then use next_call to activate this version and deactivate siblings. Activation is not a data rollback. For a transforming change: patch with /COLLECTION/IsActive=false, register the migration between source and new version, then activate and materialize."),
        ["migration"] => Ok("A migration lens transforms document values between collection versions. Read help migration TOPIC: workflow, authoring, build, contract, arguments, inverse, versions, verify, recovery. Register compiled modules with migration set; use migration set --help for its inputs. This tool does not compile source code."),
        ["migration", "set"] => Ok(r#"migration preview set: options.config is DefraDB LensConfig: {"SourceCollectionVersionID":"SOURCE","DestinationCollectionVersionID":"DESTINATION","Lenses":[{"Module":"BASE64_WASM","Arguments":{},"Inverse":false}]}. Omit target_id. Discover exact version IDs with version list. Versions must exist and be adjacent as required by DefraDB. Modules implement the DefraDB lens WASM contract; this tool registers compiled modules, not source code. Inline Module bytes only: Path is rejected because schema access grants no file access. Review the preview and use next_call; DefraDB validates publication. Inspect version get, then activate and materialize. Additive nullable fields usually need no lens."#),
        ["view"] => Ok("view create defines a query-backed collection. Read help views for virtual and materialized views; use view create --help for query and SDL inputs."),
        ["view", "create"] => Ok("view preview create: options.query is a source selection such as WorkItem { title }; options.sdl declares the target view collection. Omit target_id. Review the preview and use next_call. DefraDB validates the query and schema. Inspect the result with collection get. Views grant no access to source documents."),
        _ => bail!("unknown schema help path"),
    }
}

pub(super) fn effect(words: &[&str]) -> &'static str {
    match words {
        ["collection", "create"] => "Install new collection definitions; matching definitions are retained.",
        ["collection", "update"] => "Patch the collection definition. DefraDB may create and activate a new version; removing or changing fields can affect readers and writers. Existing data values are not backfilled.",
        ["version", "activate"] => "Switch the active version and reindex through registered migrations; this is not a document rollback.",
        ["migration", "set"] => "Register the inline WASM transformation between the specified versions.",
        ["view", "create"] => "Create a view from the source selection and target schema.",
        ["collection", "materialize"] => "Migrate and cache known documents at the active version, without new document commits.",
        _ => "Inspect the command help.",
    }
}
