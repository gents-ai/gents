use std::collections::BTreeSet;

use serde_json::json;

use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;

use super::{
    read_record, remove_record_in_txn, DriftPolicy, InstallationError, RemoveReport, RECORD,
};

type Result<T, E = InstallationError> = std::result::Result<T, E>;

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
        .execute_with_variables(
            &format!(
                r#"mutation($input: {RECORD}MutationInputArg!) {{ update_{RECORD}(docID: "{}", input: $input) {{ _docID }} }}"#,
                escape_graphql_string(doc_id)
            ),
            &json!({ "input": { "required_by": value } }),
        )
        .await?;
    if !response
        .get("errors")
        .is_none_or(serde_json::Value::is_null)
    {
        return Err(InstallationError::RequiredByUpdateFailed {
            errors: response["errors"].clone(),
        });
    }
    Ok(())
}

pub(super) async fn claim_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    dependent: &str,
    dependencies: &[String],
    policy: DriftPolicy,
) -> Result<()> {
    for coordinate in dependencies {
        let record = read_record(txn, owner, coordinate).await?;
        let doc_id = record
            .doc_id
            .ok_or_else(|| InstallationError::NotInstalled {
                coordinate: coordinate.to_owned(),
                owner: owner.to_owned(),
            })?;
        let mut required_by = record.required_by;
        if required_by.insert(dependent.to_owned()) {
            update_required_by(txn, &doc_id, &required_by).await?;
        }
    }
    let response = txn
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID coordinate required_by explicit }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    for row in response["data"][RECORD]
        .as_array()
        .ok_or(InstallationError::RecordsRowsMissing)?
        .clone()
    {
        let coordinate = row["coordinate"]
            .as_str()
            .ok_or(InstallationError::RecordMissingCoordinate)?
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
            remove_record_in_txn(txn, owner, &coordinate, &record, policy).await?;
        } else {
            let doc_id = row["_docID"]
                .as_str()
                .ok_or(InstallationError::RecordMissingDocId)?;
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
) -> Result<Vec<RemoveReport>> {
    let response = txn
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID coordinate required_by explicit }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    let rows = response["data"][RECORD]
        .as_array()
        .ok_or(InstallationError::RecordsRowsMissing)?
        .clone();
    let mut released = Vec::new();
    for row in &rows {
        let coordinate = row["coordinate"]
            .as_str()
            .ok_or(InstallationError::RecordMissingCoordinate)?
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
            released.push(remove_record_in_txn(txn, owner, &coordinate, &record, policy).await?);
        } else {
            let doc_id = row["_docID"]
                .as_str()
                .ok_or(InstallationError::RecordMissingDocId)?;
            update_required_by(txn, doc_id, &required_by).await?;
        }
    }
    Ok(released)
}
