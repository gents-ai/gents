use anyhow::Context as _;
use serde_json::json;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};

pub const SCHEMA: &str = "type WriterCounter { key: String @index(unique: true) value: Int }
type WriterReceipt { writer: Int iteration: Int }";
pub const INCREMENT: &str = r#"mutation Increment($value: Int!, $writer: Int!, $iteration: Int!) {
            update_WriterCounter(filter: {key: {_eq: "hot"}}, input: {value: $value}) {_docID}
            create_WriterReceipt(input: {writer: $writer, iteration: $iteration}) {_docID}
        }"#;
pub const OBSERVE: &str = "{ WriterCounter {value} WriterReceipt {writer iteration} }";

pub async fn seed(access: &ConfigAccess) -> anyhow::Result<()> {
    access
        .write(
            "writer_probe.seed",
            r#"mutation { create_WriterCounter(input: {key: "hot", value: 0}) {_docID} }"#,
        )
        .await?;
    Ok(())
}

pub async fn read(txn: &ConfigApplyTxn<'_>) -> anyhow::Result<i64> {
    let row = txn.execute("{ WriterCounter {value} }").await?;
    row["data"]["WriterCounter"][0]["value"]
        .as_i64()
        .context("counter absent")
}

pub async fn increment(
    txn: &ConfigApplyTxn<'_>,
    previous: i64,
    writer: i64,
    iteration: i64,
) -> anyhow::Result<()> {
    txn.execute_with_variables(
        INCREMENT,
        &json!({"value": previous + 1, "writer": writer, "iteration": iteration}),
    )
    .await?;
    Ok(())
}
