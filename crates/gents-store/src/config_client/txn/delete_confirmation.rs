use serde_json::Value;

use super::ConfigClientError;

pub(super) fn confirm_configuration_delete(
    response: &Value,
    collection: &str,
    doc_id: &str,
) -> Result<(), ConfigClientError> {
    let field = format!("delete_{collection}");
    let rows = crate::graphql::mutation_documents(response, &field)?;
    if matches!(rows, Some([row]) if row.get("_docID").and_then(Value::as_str) == Some(doc_id)) {
        Ok(())
    } else {
        Err(ConfigClientError::ClientConfigurationDeleteNotApplied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pack_delete_confirmation_matches_modeled_physical_identity() {
        gents_lean_vocab::families::household_pack_removal_confirmation::replay(|input| {
            let returned = if input.returned_matches {
                "pack"
            } else {
                "other"
            };
            let response = json!({"data": {"delete_PackInstallation": [{"_docID": returned}]}});
            confirm_configuration_delete(&response, "PackInstallation", "pack").is_ok()
        });
        for response in [
            json!({"data": {"delete_PackInstallation": []}}),
            json!({"data": {"delete_PackInstallation": [{"_docID":"pack"}, {"_docID":"pack"}]}}),
            json!({"errors":[{"message":"failed"}], "data": {"delete_PackInstallation": [{"_docID":"pack"}]}}),
        ] {
            assert!(confirm_configuration_delete(&response, "PackInstallation", "pack").is_err());
        }
    }
}
