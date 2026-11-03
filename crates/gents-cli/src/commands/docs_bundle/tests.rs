use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;

use clap::{CommandFactory, Parser};
use gents::Collection;
use serde_json::{json, Value};

use super::config_reference::document_id;
use super::{docs_bundle, BundleFiles, BundleRelease};
use crate::Cli;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn export(out: &Path, archive: &Path) {
    let tag = tag();
    let argv = [
        "gents",
        "__docs-bundle",
        "--out",
        out.to_str().expect("utf-8 temp path"),
        "--archive",
        archive.to_str().expect("utf-8 temp path"),
        "--tag",
        &tag,
        "--commit",
        COMMIT,
    ];
    let crate::Command::DocsBundle(args) = Cli::try_parse_from(argv).expect("parse").command else {
        panic!("__docs-bundle parsed as another command");
    };
    docs_bundle(args).expect("export the docs bundle");
}

fn read_tree(root: &Path) -> BundleFiles {
    fn walk(root: &Path, dir: &Path, files: &mut BundleFiles) {
        for entry in fs::read_dir(dir).expect("read bundle dir") {
            let path = entry.expect("bundle entry").path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let relative = path.strip_prefix(root).expect("inside root");
                let relative = relative.to_str().expect("utf-8 path").replace('\\', "/");
                files.insert(relative, fs::read(&path).expect("read bundle file"));
            }
        }
    }
    let mut files = BundleFiles::new();
    walk(root, root, &mut files);
    files
}

fn read_archive(path: &Path) -> BundleFiles {
    let bytes = fs::read(path).expect("read archive");
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    let mut files = BundleFiles::new();
    for entry in archive.entries().expect("archive entries") {
        let mut entry = entry.expect("archive entry");
        let path = entry
            .path()
            .expect("entry path")
            .to_str()
            .expect("utf-8")
            .to_owned();
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).expect("entry contents");
        files.insert(path, contents);
    }
    files
}

fn json_file(files: &BundleFiles, path: &str) -> Value {
    let bytes = files
        .get(path)
        .unwrap_or_else(|| panic!("bundle has no {path}"));
    serde_json::from_slice(bytes).unwrap_or_else(|error| panic!("{path} is not JSON: {error}"))
}

/// Every non-hidden declared command path paired with its non-hidden argument ids.
fn declared_commands(
    command: &clap::Command,
    path: Vec<String>,
    out: &mut Vec<(Vec<String>, Vec<String>)>,
) {
    let args = command
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .map(|arg| arg.get_id().to_string())
        .collect();
    out.push((path.clone(), args));
    for subcommand in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        let mut path = path.clone();
        path.push(subcommand.get_name().to_owned());
        declared_commands(subcommand, path, out);
    }
}

fn exported_commands(command: &Value, out: &mut Vec<(Vec<String>, Vec<String>)>) {
    let strings = |value: &Value, key: &str| -> Vec<String> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("{key} is not an array"))
            .iter()
            .map(|item| {
                let item = if key == "args" { &item["id"] } else { item };
                item.as_str().expect("string").to_owned()
            })
            .collect()
    };
    out.push((
        strings(&command["path"], "path"),
        strings(&command["args"], "args"),
    ));
    for subcommand in command["subcommands"].as_array().expect("subcommands") {
        exported_commands(subcommand, out);
    }
}

fn defs_ref(property: &Value) -> Option<&str> {
    property
        .get("$ref")
        .or_else(|| property.get("items").and_then(|items| items.get("$ref")))
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
}

#[test]
fn exporter_writes_a_complete_deterministic_bundle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (first, second) = (temp.path().join("first"), temp.path().join("second"));
    export(&first, &temp.path().join("first.tar.gz"));
    export(&second, &temp.path().join("second.tar.gz"));

    let files = read_tree(&first);
    assert_eq!(files, read_tree(&second), "re-export changed the bundle");
    assert_eq!(
        fs::read(temp.path().join("first.tar.gz")).expect("archive"),
        fs::read(temp.path().join("second.tar.gz")).expect("archive"),
        "re-export changed the archive bytes"
    );
    assert_eq!(
        read_archive(&temp.path().join("first.tar.gz")),
        files,
        "the archive must hold exactly the bundle files at its root"
    );

    assert_eq!(
        json_file(&files, "manifest.json"),
        json!({
            "schema": 1,
            "tag": tag(),
            "version": env!("CARGO_PKG_VERSION"),
            "commit": COMMIT,
        })
    );

    let cli = json_file(&files, "cli.json");
    assert_eq!(cli["name"], "gents");
    assert_eq!(cli["version"], env!("CARGO_PKG_VERSION"));
    let mut declared = Vec::new();
    declared_commands(&Cli::command(), vec!["gents".to_owned()], &mut declared);
    let mut exported = Vec::new();
    exported_commands(&cli, &mut exported);
    assert_eq!(
        exported, declared,
        "cli.json must hold exactly the non-hidden commands and arguments"
    );
    assert!(
        !exported
            .iter()
            .any(|(path, _)| path.iter().any(|name| name.starts_with("__"))),
        "hidden commands leaked into cli.json"
    );

    let index = json_file(&files, "config/index.json");
    let index = index.as_array().expect("config index is an array");
    let mut ids = BTreeSet::new();
    for entry in index {
        let id = entry["id"].as_str().expect("index id");
        assert!(ids.insert(id.to_owned()), "duplicate config id {id}");
        let schema_path = entry["schema"].as_str().expect("index schema path");
        assert_eq!(schema_path, format!("config/{id}.schema.json"));
        let schema = json_file(&files, schema_path);
        assert!(schema.is_object(), "{schema_path} is not a schema object");
        assert_eq!(entry["title"], schema["title"], "{id} title");
        assert_eq!(
            entry["description"],
            schema.get("description").cloned().unwrap_or(Value::Null),
            "{id} description"
        );
    }
    let mut expected: BTreeSet<_> = Collection::ALL.into_iter().map(document_id).collect();
    expected.extend(["pack_config".to_owned(), "pack_manifest".to_owned()]);
    assert_eq!(ids, expected);

    let pack_config = json_file(&files, "config/pack_config.schema.json");
    let properties = pack_config["properties"]
        .as_object()
        .expect("pack_config properties");
    let roots: BTreeSet<_> = Collection::ALL
        .into_iter()
        .map(|collection| collection.dir_name().unwrap_or("node"))
        .collect();
    assert_eq!(
        properties
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        roots,
        "every pack_config root is one canonical collection"
    );
    for collection in Collection::ALL {
        let root = collection.dir_name().unwrap_or("node");
        let schema = json_file(
            &files,
            &format!("config/{}.schema.json", document_id(collection)),
        );
        assert_eq!(
            defs_ref(&properties[root]),
            schema["title"].as_str(),
            "pack_config.{root} must hold the {collection} document schema"
        );
    }
}

#[test]
fn release_tag_must_name_this_binary_version() {
    let commit = Some(COMMIT.to_owned());
    assert!(BundleRelease::resolve(Some("v0.0.0".into()), commit.clone()).is_err());
    assert!(BundleRelease::resolve(Some(tag()), Some("abc123".into())).is_err());
    assert_eq!(
        BundleRelease::resolve(Some(tag()), commit).expect("valid release"),
        BundleRelease {
            tag: tag(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            commit: COMMIT.to_owned(),
        }
    );
}

#[test]
fn export_refuses_a_non_empty_output_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("stale.json"), b"{}").expect("seed stale file");
    let tag = tag();
    let argv = [
        "gents",
        "__docs-bundle",
        "--out",
        temp.path().to_str().expect("utf-8 temp path"),
        "--tag",
        &tag,
        "--commit",
        COMMIT,
    ];
    let crate::Command::DocsBundle(args) = Cli::try_parse_from(argv).expect("parse").command else {
        panic!("__docs-bundle parsed as another command");
    };
    assert!(docs_bundle(args).is_err());
}
