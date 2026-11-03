use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents::store::graphql::{
    escape_graphql_string, validate_collection_identifier, validate_graphql_name,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::cli::args::ExportArgs;

pub(super) struct ExportFile {
    collection: String,
    rows: u64,
    bytes: u64,
    sha256: String,
    file: NamedTempFile,
}

pub(crate) async fn export(args: ExportArgs) -> Result<()> {
    ensure!(args.jsonl, "pass --jsonl to select the archive format");
    ensure!(
        args.all_app_collections || !args.collections.is_empty(),
        "select collections or pass --all-app-collections"
    );
    let (access, _) = crate::resolve_config_access(args.home.as_deref(), None).await?;
    let collections: BTreeSet<String> = if args.all_app_collections {
        app_collection_names(&access).await?
    } else {
        args.collections.into_iter().collect()
    };
    for collection in &collections {
        validate_collection_identifier(collection)?;
    }
    let filter = match args.filter {
        Some(raw) => {
            let value: Value = serde_json::from_str(&raw).context("parsing --filter as JSON")?;
            let rendered = gents::tools::builtin::defra_query::render_filter(&value)?;
            rendered[1..rendered.len() - 1].to_owned()
        }
        None => String::new(),
    };
    std::fs::create_dir_all(&args.output_dir).context("creating export directory")?;
    let output_dir = args.output_dir;
    for name in collections
        .iter()
        .map(|name| format!("{name}.jsonl"))
        .chain(std::iter::once("MANIFEST.tsv".into()))
    {
        ensure!(
            !output_dir.join(&name).exists(),
            "export destination already exists: {name}"
        );
    }
    let files = access
        .transact_readonly("cli.export.snapshot", |txn| {
            let collections = &collections;
            let filter = &filter;
            let output_dir = &output_dir;
            Box::pin(async move { stage_export(txn, collections, filter, output_dir).await })
        })
        .await?;
    if args.all_app_collections {
        ensure!(
            app_collection_names(&access).await? == collections,
            "application collections changed during export; retry to include the current set"
        );
    }
    let count = files.len();
    publish(files, &output_dir)?;
    crate::print_json(&serde_json::json!({
        "status": "complete",
        "output_dir": output_dir,
        "collections": count,
        "scope": "documents visible to the home node"
    }))
}

async fn app_collection_names(access: &ConfigAccess) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for name in access.collection_names().await? {
        if name.starts_with('_')
            || gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&name.as_str())
        {
            continue;
        }
        let version = access
            .collection_version(&name)
            .await?
            .with_context(|| format!("collection {name:?} changed during discovery"))?;
        if version.get("Query").is_none_or(Value::is_null) {
            result.insert(name);
        }
    }
    Ok(result)
}

pub(super) async fn stage_export(
    txn: &ConfigApplyTxn<'_>,
    collections: &BTreeSet<String>,
    filter: &str,
    output_dir: &Path,
) -> Result<Vec<ExportFile>> {
    let mut files = Vec::with_capacity(collections.len());
    for collection in collections {
        let query = format!(
            r#"{{ __type(name: "{}") {{ fields {{ name type {{ kind name ofType {{ kind name ofType {{ kind name ofType {{ kind name }} }} }} }} }} }} }}"#,
            escape_graphql_string(collection)
        );
        let schema = txn.execute(&query).await?;
        let selection = field_selection(&schema["data"]["__type"], collection)?;
        let mut file =
            NamedTempFile::new_in(output_dir).context("creating temporary export file")?;
        let mut writer = DigestWriter::new(BufWriter::new(file.as_file_mut()));
        let mut rows = 0u64;
        txn.visit_cursor_pages(collection, &selection.query, filter, |page| {
            for row in page {
                let object = row.as_object().context("export row is not an object")?;
                for field in &selection.fields {
                    ensure!(
                        object.contains_key(field),
                        "export row omitted {collection}.{field}"
                    );
                }
                for relation in &selection.relations {
                    validate_relation(object, collection, relation)?;
                }
                serde_json::to_writer(&mut writer, row)?;
                writer.write_all(b"\n")?;
                rows = rows.checked_add(1).context("export row count overflow")?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await?;
        writer.flush()?;
        let bytes = writer.bytes;
        let sha256 = format!("{:x}", writer.hash.clone().finalize());
        drop(writer);
        files.push(ExportFile {
            collection: collection.clone(),
            rows,
            bytes,
            sha256,
            file,
        });
    }
    Ok(files)
}

struct FieldSelection {
    query: String,
    fields: Vec<String>,
    relations: Vec<String>,
}

fn field_selection(schema: &Value, collection: &str) -> Result<FieldSelection> {
    let fields = schema
        .get("fields")
        .and_then(Value::as_array)
        .with_context(|| format!("collection {collection:?} has no accessible schema"))?;
    let mut selected = BTreeMap::new();
    selected.insert("_docID".to_owned(), false);
    for field in fields {
        let name = field
            .get("name")
            .and_then(Value::as_str)
            .context("schema field has no name")?;
        if matches!(name, "_deleted" | "_version")
            || matches!(
                name,
                "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "GROUP" | "SIMILARITY" | "BM25"
            )
        {
            continue;
        }
        validate_graphql_name(name)?;
        let mut ty = field.get("type").context("schema field has no type")?;
        loop {
            match ty.get("kind").and_then(Value::as_str) {
                Some("NON_NULL" | "LIST") => {
                    ty = ty
                        .get("ofType")
                        .filter(|value| !value.is_null())
                        .with_context(|| {
                            format!("unsupported nested type for {collection}.{name}")
                        })?
                }
                Some("SCALAR" | "ENUM") => {
                    selected.insert(name.to_owned(), false);
                    break;
                }
                Some("OBJECT") => {
                    selected.insert(name.to_owned(), true);
                    break;
                }
                Some(kind) => bail!("cannot export {collection}.{name} with type {kind}"),
                None => bail!("cannot determine type of {collection}.{name}"),
            }
        }
    }
    let query = selected
        .iter()
        .map(|(name, relation)| {
            if *relation {
                format!("{name} {{ _docID }}")
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let fields = selected.keys().cloned().collect();
    let relations = selected
        .into_iter()
        .filter_map(|(name, relation)| relation.then_some(name))
        .collect();
    Ok(FieldSelection {
        query,
        fields,
        relations,
    })
}

fn validate_relation(
    object: &serde_json::Map<String, Value>,
    collection: &str,
    relation: &str,
) -> Result<()> {
    let value = &object[relation];
    let validate_target = |target: &Value| -> Result<()> {
        ensure!(
            target
                .get("_docID")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty()),
            "export could not resolve a reference in {collection}.{relation}"
        );
        Ok(())
    };
    match value {
        Value::Null => {
            let backing = format!("_{relation}ID");
            ensure!(
                object.get(&backing).is_none_or(Value::is_null),
                "export could not resolve the stored reference in {collection}.{relation}"
            );
        }
        Value::Array(targets) => {
            for target in targets {
                validate_target(target)?;
            }
        }
        Value::Object(_) => {
            validate_target(value)?;
            let backing = format!("_{relation}ID");
            if let Some(id) = object.get(&backing).and_then(Value::as_str) {
                ensure!(
                    value["_docID"].as_str() == Some(id),
                    "export found inconsistent references in {collection}.{relation}"
                );
            }
        }
        _ => bail!("unexpected reference value in {collection}.{relation}"),
    }
    Ok(())
}

pub(super) fn publish(files: Vec<ExportFile>, output_dir: &Path) -> Result<()> {
    let mut manifest = NamedTempFile::new_in(output_dir)?;
    writeln!(manifest, "collection\trows\tbytes\tsha256")?;
    for file in &files {
        writeln!(
            manifest,
            "{}\t{}\t{}\t{}",
            file.collection, file.rows, file.bytes, file.sha256
        )?;
    }
    manifest.flush()?;
    for file in files {
        let path = output_dir.join(format!("{}.jsonl", file.collection));
        file.file
            .persist_noclobber(&path)
            .map_err(|error| error.error)
            .with_context(|| format!("publishing {}", path.display()))?;
    }
    manifest
        .persist_noclobber(output_dir.join("MANIFEST.tsv"))
        .map_err(|error| error.error)
        .context("publishing export manifest")?;
    Ok(())
}

struct DigestWriter<W> {
    inner: W,
    hash: Sha256,
    bytes: u64,
}

impl<W> DigestWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
            bytes: 0,
        }
    }
}

impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hash.update(&buf[..written]);
        self.bytes = self
            .bytes
            .checked_add(written as u64)
            .ok_or_else(|| std::io::Error::other("export byte count overflow"))?;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_selection_preserves_backing_ids_and_relationships() {
        let schema = json!({"fields":[
            {"name":"body","type":{"kind":"SCALAR","name":"String"}},
            {"name":"tags","type":{"kind":"LIST","ofType":{"kind":"SCALAR","name":"String"}}},
            {"name":"_docID","type":{"kind":"SCALAR","name":"ID"}},
            {"name":"_authorID","type":{"kind":"SCALAR","name":"ID"}},
            {"name":"author","type":{"kind":"OBJECT","name":"Person"}},
            {"name":"children","type":{"kind":"LIST","ofType":{"kind":"OBJECT","name":"Record"}}},
            {"name":"COUNT","type":{"kind":"SCALAR","name":"Int"}}
        ]});
        assert_eq!(
            field_selection(&schema, "Record").unwrap().query,
            "_authorID _docID author { _docID } body children { _docID } tags"
        );
        let unresolved = json!({"author":null,"_authorID":"hidden"});
        assert!(validate_relation(unresolved.as_object().unwrap(), "Record", "author").is_err());
        let matching = json!({"author":{"_docID":"known"},"_authorID":"known"});
        assert!(validate_relation(matching.as_object().unwrap(), "Record", "author").is_ok());
        let mismatch = json!({"author":{"_docID":"other"},"_authorID":"known"});
        assert!(validate_relation(mismatch.as_object().unwrap(), "Record", "author").is_err());
    }

    #[test]
    fn publishing_never_overwrites_and_manifest_matches_bytes() {
        let output = tempfile::tempdir().unwrap();
        let mut file = NamedTempFile::new_in(output.path()).unwrap();
        let contents = "{\"body\":\"full value\"}\n";
        file.write_all(contents.as_bytes()).unwrap();
        let digest = format!("{:x}", Sha256::digest(contents.as_bytes()));
        let item = ExportFile {
            collection: "Record".into(),
            rows: 1,
            bytes: contents.len() as u64,
            sha256: digest.clone(),
            file,
        };
        publish(vec![item], output.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(output.path().join("Record.jsonl")).unwrap(),
            contents
        );
        assert_eq!(
            std::fs::read_to_string(output.path().join("MANIFEST.tsv")).unwrap(),
            format!(
                "collection\trows\tbytes\tsha256\nRecord\t1\t{}\t{digest}\n",
                contents.len()
            )
        );
        let mut other = NamedTempFile::new_in(output.path()).unwrap();
        other.write_all(b"new data").unwrap();
        let item = ExportFile {
            collection: "Record".into(),
            rows: 1,
            bytes: 8,
            sha256: "ignored".into(),
            file: other,
        };
        assert!(publish(vec![item], output.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(output.path().join("Record.jsonl")).unwrap(),
            contents
        );
    }

    #[test]
    fn digest_writer_counts_exact_utf8_bytes_without_shortening_values() {
        let text = format!("{}😀", "é".repeat(1500));
        let mut writer = DigestWriter::new(Vec::new());
        serde_json::to_writer(&mut writer, &json!({"body": text})).unwrap();
        writer.write_all(b"\n").unwrap();
        assert_eq!(writer.bytes, writer.inner.len() as u64);
        assert_eq!(
            writer.hash.clone().finalize().as_slice(),
            Sha256::digest(&writer.inner).as_slice()
        );
        let row: Value = serde_json::from_slice(&writer.inner).unwrap();
        assert_eq!(row["body"], text);
    }
}
