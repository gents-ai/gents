use std::collections::BTreeSet;
use std::sync::Arc;

use gents::store::config_client::ConfigAccess;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::export::{publish, stage_export};

#[tokio::test]
async fn native_export_keeps_long_fields_and_all_cursor_pages() {
    let node = Arc::new(gents::defra_node::Node::builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    access
        .add_collection("type ExportRecord { body: String group: String }")
        .await
        .unwrap();
    let long = format!("{}😀", "é".repeat(1500));
    let escaped = gents::store::graphql::escape_graphql_string(&long);
    let inputs = (0..70)
        .map(|index| {
            format!(
                "{{body:\"{escaped}\",group:\"{}\"}}",
                if index == 69 { "skip" } else { "keep" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    ConfigAccess::write_local(
        &node,
        "test.export.seed",
        &format!("mutation {{ create_ExportRecord(input:[{inputs}]) {{_docID}} }}"),
    )
    .await
    .unwrap();

    let output = tempfile::tempdir().unwrap();
    let collections = BTreeSet::from(["ExportRecord".to_string()]);
    let files = access
        .transact_readonly("test.export.snapshot", |txn| {
            let collections = &collections;
            let directory = output.path();
            Box::pin(async move {
                stage_export(txn, collections, "group: {_eq: \"keep\"}", directory).await
            })
        })
        .await
        .unwrap();
    publish(files, output.path()).unwrap();
    let bytes = std::fs::read(output.path().join("ExportRecord.jsonl")).unwrap();
    let rows: Vec<Value> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 69);
    for row in &rows {
        assert_eq!(row["body"], long);
        assert_eq!(row["group"], "keep");
        assert!(row["_docID"].as_str().is_some());
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        std::fs::read_to_string(output.path().join("MANIFEST.tsv")).unwrap(),
        format!(
            "collection\trows\tbytes\tsha256\nExportRecord\t69\t{}\t{hash}\n",
            bytes.len()
        )
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn native_export_retains_relationship_ids() {
    let node = Arc::new(gents::defra_node::Node::builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    access.add_collection("type ExportAuthor { name: String books: [ExportBook] }\ntype ExportBook { title: String author: ExportAuthor }").await.unwrap();
    let author = ConfigAccess::write_local(
        &node,
        "test.export.author",
        r#"mutation { create_ExportAuthor(input:{name:"Author"}) {_docID} }"#,
    )
    .await
    .unwrap();
    let author_id = author["data"]["create_ExportAuthor"][0]["_docID"]
        .as_str()
        .unwrap();
    let mutation = format!(
        r#"mutation {{ create_ExportBook(input:{{title:"Book",_authorID:"{}"}}) {{_docID}} }}"#,
        gents::store::graphql::escape_graphql_string(author_id)
    );
    let book = ConfigAccess::write_local(&node, "test.export.book", &mutation)
        .await
        .unwrap();
    let book_id = book["data"]["create_ExportBook"][0]["_docID"]
        .as_str()
        .unwrap();
    let output = tempfile::tempdir().unwrap();
    let collections = BTreeSet::from(["ExportAuthor".to_owned(), "ExportBook".to_owned()]);
    let files = access
        .transact_readonly("test.export.relationship", |txn| {
            let collections = &collections;
            let directory = output.path();
            Box::pin(async move { stage_export(txn, collections, "", directory).await })
        })
        .await
        .unwrap();
    publish(files, output.path()).unwrap();
    let author_row: Value = serde_json::from_str(
        std::fs::read_to_string(output.path().join("ExportAuthor.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    let book_row: Value = serde_json::from_str(
        std::fs::read_to_string(output.path().join("ExportBook.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(book_row["_authorID"], author_id);
    assert_eq!(book_row["author"]["_docID"], author_id);
    assert_eq!(author_row["books"][0]["_docID"], book_id);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn native_export_preserves_fields_beyond_selection_width_limit() {
    let node = Arc::new(gents::defra_node::Node::builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    let fields = (0..130)
        .map(|index| format!("f_{index}: String"))
        .collect::<Vec<_>>()
        .join(" ");
    access
        .add_collection(&format!("type ExportWide {{ {fields} }}"))
        .await
        .unwrap();
    let values = (0..130)
        .map(|index| format!("f_{index}:\"value_{index}\""))
        .collect::<Vec<_>>()
        .join(",");
    ConfigAccess::write_local(
        &node,
        "test.export.wide.seed",
        &format!("mutation {{ create_ExportWide(input:{{{values}}}) {{_docID}} }}"),
    )
    .await
    .unwrap();
    let output = tempfile::tempdir().unwrap();
    let collections = BTreeSet::from(["ExportWide".to_owned()]);
    let files = access
        .transact_readonly("test.export.wide", |txn| {
            let collections = &collections;
            let directory = output.path();
            Box::pin(async move { stage_export(txn, collections, "", directory).await })
        })
        .await
        .unwrap();
    publish(files, output.path()).unwrap();
    let row: Value = serde_json::from_str(
        std::fs::read_to_string(output.path().join("ExportWide.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    for index in 0..130 {
        assert_eq!(row[format!("f_{index}")], format!("value_{index}"));
    }
    node.shutdown().await.expect("node shutdown");
}
