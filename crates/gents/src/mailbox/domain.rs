use super::*;

const CORRELATION_FIELD: &str = "mailbox_item_key";

// This validates correlation, not response authority or approval. DefraDB ACP
// owns document writes; the domain workflow interprets the response contents.
pub(super) fn response_field(
    node: &EmbeddedNode,
    collection: &str,
    entries: &[MailboxCloseCollection],
) -> Result<&'static str> {
    validate_collection_identifier(collection)?;
    let field = mailbox_close_collection(entries, collection)
        .map_or(CORRELATION_FIELD, |entry| entry.correlation_field);
    let version = node
        .get_collection(collection)?
        .map(serde_json::to_value)
        .transpose()?
        .with_context(|| format!("mailbox response collection {collection:?} does not exist"))?;
    validate_response_schema(&version, field)
        .with_context(|| format!("invalid mailbox response collection {collection:?}"))?;
    Ok(field)
}

fn validate_response_schema(version: &Value, field: &str) -> Result<()> {
    validate_graphql_name(field)?;
    let schema = query::parse_sdl(&format!(
        "type MailboxResponseContract {{ {field}: String @immutable @index(unique: true) }}"
    ))?;
    let expected = serde_json::to_value(&schema[0])?;
    let find_field = |value: &Value| {
        value["Fields"]
            .as_array()
            .and_then(|fields| fields.iter().find(|entry| entry["Name"] == field).cloned())
    };
    let actual = find_field(version).context("missing mailbox_item_key correlation field")?;
    let expected = find_field(&expected).context("missing response contract field")?;
    anyhow::ensure!(
        actual["Kind"] == expected["Kind"] && actual["Immutable"] == true,
        "mailbox correlation field must be an immutable String"
    );
    anyhow::ensure!(
        version["Indexes"].as_array().is_some_and(|indexes| {
            indexes.iter().any(|index| {
                index["Unique"] == true
                    && index["Fields"]
                        .as_array()
                        .is_some_and(|fields| fields.len() == 1 && fields[0]["Name"] == field)
            })
        }),
        "mailbox correlation field requires its own unique index"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installed_domain_schema_resolves_document_response_without_static_registration() {
        let node = super::super::tests::test_node().await;
        let item = stamp_create(
            &node,
            &super::super::tests::context("did:test:owner"),
            super::super::tests::args(MailboxAction::WriteDocument, "domain-response"),
        )
        .await
        .unwrap();
        assert_eq!(sweep_open_mailbox_items(&node).await.unwrap().acted, 0);
        let response = node.execute(&format!(
            r#"mutation {{ create_MailboxFixture(input: {{ mailbox_item_key: "{}", body: "declined" }}) {{ _docID }} }}"#,
            escape_graphql_string(&item.item_key)
        )).await;
        assert!(!response.has_errors(), "{response:?}");
        let response_id = single_mutation_document(&response, "create_MailboxFixture")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(sweep_open_mailbox_items(&node).await.unwrap().acted, 1);
        let resolved = load_mailbox_item(&node, &item.doc_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.status, "acted");
        assert_eq!(
            resolved.resolved_doc_id.as_deref(),
            Some(response_id.as_str())
        );
        assert_eq!(sweep_open_mailbox_items(&node).await.unwrap().acted, 0);
    }

    #[test]
    fn response_schema_requires_immutable_unique_string_correlation() {
        for (fields, accepted) in [
            (
                "mailbox_item_key: String @immutable @index(unique: true)",
                true,
            ),
            ("mailbox_item_key: String @index(unique: true)", false),
            ("mailbox_item_key: String @immutable", false),
            (
                "mailbox_item_key: Int @immutable @index(unique: true)",
                false,
            ),
            ("message: String", false),
        ] {
            let schema = query::parse_sdl(&format!("type Decision {{ {fields} }}")).unwrap();
            let version = serde_json::to_value(&schema[0]).unwrap();
            assert_eq!(
                validate_response_schema(&version, CORRELATION_FIELD).is_ok(),
                accepted,
                "{fields}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_response_schema_cannot_create_an_attention_item() {
        let node = super::super::tests::test_node().await;
        node.add_schema("type InvalidResponse { mailbox_item_key: String }")
            .await
            .unwrap();
        let context = super::super::tests::context("did:test:owner");
        for collection in ["InvalidResponse", "MissingResponse"] {
            let mut args = super::super::tests::args(MailboxAction::WriteDocument, collection);
            args.expected_collection = Some(collection.into());
            assert!(stamp_create(&node, &context, args).await.is_err());
        }
        assert!(list_mailbox_items(&node, &context.requester_did, None)
            .await
            .unwrap()
            .is_empty());
    }
}
