use serde_json::Value;

use super::ConfigClientError;

pub(super) fn confirm_configuration_delete(
    response: &Value,
    collection: &str,
    doc_id: &str,
) -> Result<(), ConfigClientError> {
    let field = format!("delete_{collection}");
    let rows = crate::store::graphql::mutation_documents(response, &field)?;
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
        let modeled = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration policy model");
        assert_eq!(modeled.configuration_pack_removal_cases.len(), 5);
        for name in ["agent-owner", "wrong-document"] {
            let case = modeled
                .configuration_pack_removal_cases
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled removal case");
            let returned = if case["returned_matches"] == true {
                "pack"
            } else {
                "other"
            };
            let response = json!({"data": {"delete_PackInstallation": [{"_docID": returned}]}});
            assert_eq!(
                confirm_configuration_delete(&response, "PackInstallation", "pack").is_ok(),
                case["expected"].as_bool().unwrap(),
                "{name}"
            );
        }
        for response in [
            json!({"data": {"delete_PackInstallation": []}}),
            json!({"data": {"delete_PackInstallation": [{"_docID":"pack"}, {"_docID":"pack"}]}}),
            json!({"errors":[{"message":"failed"}], "data": {"delete_PackInstallation": [{"_docID":"pack"}]}}),
        ] {
            assert!(confirm_configuration_delete(&response, "PackInstallation", "pack").is_err());
        }
    }
}
