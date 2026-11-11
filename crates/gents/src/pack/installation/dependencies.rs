use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde_json::json;

use crate::store::config_client::ConfigApplyTxn;
use crate::store::graphql::escape_graphql_string;

use super::{
    read_record, remove_record_in_txn, DriftPolicy, PackChangeError, RemoveReport, RECORD,
};

async fn update_required_by(
    txn: &ConfigApplyTxn<'_>,
    doc_id: &str,
    required_by: &BTreeSet<String>,
) -> Result<()> {
    let value = if required_by.is_empty() {
        serde_json::Value::Null
    } else {
        json!(required_by.iter().collect::<Vec<_>>())
    };
    let response = txn
        .execute_client_signed_update(
            &format!(
                r#"mutation($input: {RECORD}MutationInputArg!) {{ update_{RECORD}(docID: "{}", input: $input) {{ _docID }} }}"#,
                escape_graphql_string(doc_id)
            ),
            &json!({ "input": { "required_by": value } }),
            gents_protocol::schemas::PACK_INSTALLATION,
        )
        .await?;
    anyhow::ensure!(
        response
            .get("errors")
            .is_none_or(serde_json::Value::is_null),
        "updating a dependency's required_by failed: {}",
        response["errors"]
    );
    Ok(())
}

pub(super) async fn claim_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    dependent: &str,
    dependencies: &[String],
    policy: DriftPolicy,
    profile_policy: &'static dyn crate::documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<(), PackChangeError> {
    for coordinate in dependencies {
        let record = read_record(txn, owner, coordinate).await?;
        let doc_id = record
            .doc_id
            .with_context(|| format!("{coordinate} is not installed for {owner}"))?;
        let mut required_by = record.required_by;
        if required_by.insert(dependent.to_owned()) {
            update_required_by(txn, &doc_id, &required_by).await?;
        }
    }
    let response = {
        use gents_protocol::document_query::{
            fields::pack_installation as fields, Filter, Query, Selection,
        };
        let query = Query::select(
            Selection::doc_id()
                .field(fields::COORDINATE)
                .field(fields::REQUIRED_BY)
                .field(fields::EXPLICIT),
        )
        .filter(Filter::eq(fields::NODE_DID, owner))
        .render();
        txn.run(&query).await?
    };
    for row in response["data"][RECORD]
        .as_array()
        .context("reading the pack installation records")?
        .clone()
    {
        let coordinate = row["coordinate"]
            .as_str()
            .context("record missing coordinate")?
            .to_owned();
        if dependencies
            .iter()
            .any(|dependency| dependency == &coordinate)
        {
            continue;
        }
        let mut required_by: BTreeSet<String> =
            super::decode_record_field(&row, "required_by", owner, &coordinate)?;
        if !required_by.remove(dependent) {
            continue;
        }
        let explicit = row
            .get("explicit")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        if required_by.is_empty() && !explicit {
            let record = read_record(txn, owner, &coordinate).await?;
            let removed =
                remove_record_in_txn(txn, owner, &coordinate, &record, policy, profile_policy)
                    .await?;
            let (namespace, _) = crate::pack::registry::split_pack_coordinate(&coordinate);
            let plugins = removed
                .documents
                .plugins
                .iter()
                .map(|plugin| format!("{namespace}/{}", plugin.name))
                .collect();
            super::ensure_plugins_unreferenced(txn, owner, &plugins).await?;
        } else {
            let doc_id = row["_docID"].as_str().context("record missing _docID")?;
            update_required_by(txn, doc_id, &required_by).await?;
        }
    }
    Ok(())
}

pub(super) async fn release_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    dependent: &str,
    policy: DriftPolicy,
    profile_policy: &'static dyn crate::documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<Vec<RemoveReport>, PackChangeError> {
    let response = {
        use gents_protocol::document_query::{
            fields::pack_installation as fields, Filter, Query, Selection,
        };
        let query = Query::select(
            Selection::doc_id()
                .field(fields::COORDINATE)
                .field(fields::REQUIRED_BY)
                .field(fields::EXPLICIT),
        )
        .filter(Filter::eq(fields::NODE_DID, owner))
        .render();
        txn.run(&query).await?
    };
    let rows = response["data"][RECORD]
        .as_array()
        .context("reading the pack installation records")?
        .clone();
    let mut released = Vec::new();
    for row in &rows {
        let coordinate = row["coordinate"]
            .as_str()
            .context("record missing coordinate")?
            .to_owned();
        if coordinate == dependent {
            continue;
        }
        let mut required_by: BTreeSet<String> =
            super::decode_record_field(row, "required_by", owner, &coordinate)?;
        if !required_by.remove(dependent) {
            continue;
        }
        let explicit = row
            .get("explicit")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        if required_by.is_empty() && !explicit {
            let record = read_record(txn, owner, &coordinate).await?;
            released.push(
                remove_record_in_txn(txn, owner, &coordinate, &record, policy, profile_policy)
                    .await?,
            );
        } else {
            let doc_id = row["_docID"].as_str().context("record missing _docID")?;
            update_required_by(txn, doc_id, &required_by).await?;
        }
    }
    Ok(released)
}
