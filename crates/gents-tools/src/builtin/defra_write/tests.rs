use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_loop::tool::Tool;
use serde_json::json;

use super::BoundedWriteTool;
use gents_documents::{WriteToolDecl, WriteToolField, WriteToolFieldFill};
use gents_store::config_client::ConfigAccess;

#[test]
fn write_confirmation_matches_modeled_result() {
    let model = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    assert_eq!(model.configuration_datastore_write_cases.len(), 4);
    for name in ["committed", "unconfirmed"] {
        let case = model
            .configuration_datastore_write_cases
            .iter()
            .find(|case| case["name"] == name)
            .unwrap();
        let rows = if case["response_confirmed"] == true {
            json!([{ "_docID":"document" }])
        } else {
            json!([])
        };
        let response = json!({"data":{"add_ActionRequest":rows}});
        assert_eq!(
            gents_store::graphql::created_doc_id(&response, "ActionRequest").is_ok(),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
        let mailbox = json!({"data":{"create_MailboxItem":if case["response_confirmed"] == true {
            json!([{ "_docID":"mailbox" }])
        } else { json!([]) }}});
        assert_eq!(
            gents_store::graphql::created_doc_id(&mailbox, "MailboxItem").is_ok(),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    for rows in [
        json!([{"_docID":""}]),
        json!([{"_docID":"a"},{"_docID":"b"}]),
    ] {
        assert!(gents_store::graphql::created_doc_id(
            &json!({"data":{"add_ActionRequest":rows}}),
            "ActionRequest"
        )
        .is_err());
    }
    for response in [
        json!({"data":{"add_ActionRequest":[{"_docID":"a"}]},"errors":[{"message":"failed"}]}),
        json!({"data":{"add_ActionRequest":[{"_docID":"a"}],"create_ActionRequest":[{"_docID":"a"}]}}),
    ] {
        assert!(gents_store::graphql::created_doc_id(&response, "ActionRequest").is_err());
    }
}

#[test]
fn native_input_admission_matches_lean() {
    gents_lean_vocab::families::contracts_write_input::replay::<String>(|case| {
        let base = match case.expected.as_str() {
            "text" => "String",
            "integer" => "Int",
            "number" => "Float64",
            "boolean" => "Boolean",
            other => panic!("unknown kind {other}"),
        };
        let schema = format!("{base}{}", if case.nullable { "" } else { "!" });
        let value = match case.actual.as_deref() {
            None => None,
            Some("text") => Some(json!("measurement")),
            Some("integer") => Some(json!(86)),
            Some("number") => Some(json!(0.5)),
            Some("boolean") => Some(json!(false)),
            Some("array") => Some(json!([1])),
            Some("object") => Some(json!({"x":1})),
            Some("null") => Some(json!(null)),
            other => panic!("unknown kind {other:?}"),
        };
        super::input::validate_input(&schema, case.required, case.filled, value.as_ref()).is_ok()
    });
}

async fn node_with_actionrequest() -> Arc<EmbeddedNode> {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection(
        r#"
        type ActionRequest {
            drift_sig: String
            summary: String
            status: String
            run_id: String
            expected_total: String
        }
    "#,
    )
    .await
    .unwrap();
    node
}

fn decl() -> WriteToolDecl {
    WriteToolDecl {
        notification: None,
        tool_name: "request_action".into(),
        collection: "ActionRequest".into(),
        description: "Emit one ActionRequest.".into(),
        fields: vec![
            WriteToolField {
                name: "drift_sig".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "summary".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "status".into(),
                required: false,
                fill: None,
            },
        ],
        output_obligation: None,
    }
}

#[tokio::test]
async fn rejects_raw_mailbox_writer_even_when_declaration_is_canonical() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let tool = BoundedWriteTool::new(
        ConfigAccess::local(node),
        gents_documents::mailbox_write_decl::canonical_mailbox_write_decl(),
    );
    assert!(!tool.is_well_formed());
}

#[tokio::test]
async fn writes_one_bounded_doc() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(ConfigAccess::local(Arc::clone(&node)), decl());
    let out = Tool::call(
        &tool,
        serde_json::from_value(json!({
            "drift_sig": "abc", "summary": "stale host doc", "status": "open"
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    assert!(out.contains("ActionRequest"));
    let resp = node
        .execute("{ ActionRequest { drift_sig summary status } }")
        .await;
    let rows = resp
        .data
        .unwrap()
        .get("ActionRequest")
        .unwrap()
        .as_array()
        .unwrap()
        .len();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn writes_native_measurements_without_string_coercion() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection("type HostMeasurement { disk: Int healthy: Boolean }")
        .await
        .unwrap();
    let declaration = WriteToolDecl {
        notification: None,
        tool_name: "record_measurement".into(),
        collection: "HostMeasurement".into(),
        description: "Record measured host state".into(),
        fields: ["disk", "healthy"]
            .into_iter()
            .map(|name| WriteToolField {
                name: name.into(),
                required: true,
                fill: None,
            })
            .collect(),
        output_obligation: None,
    };
    let tool = BoundedWriteTool::new(ConfigAccess::local(node.clone()), declaration);
    Tool::call(
        &tool,
        serde_json::from_value(json!({"disk":86,"healthy":false})).unwrap(),
    )
    .await
    .unwrap();
    let response = node.execute("{ HostMeasurement { disk healthy } }").await;
    assert!(!response.has_errors());
    assert_eq!(
        response.data.unwrap()["HostMeasurement"][0],
        json!({"disk":86,"healthy":false})
    );
}

#[tokio::test]
async fn rejects_missing_required_and_undeclared_fields() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(ConfigAccess::local(node), decl());
    assert!(Tool::call(
        &tool,
        serde_json::from_value(json!({ "summary": "x" })).unwrap()
    )
    .await
    .is_err());
    assert!(Tool::call(
        &tool,
        serde_json::from_value(json!({
            "drift_sig": "a", "summary": "b", "evil": "1"
        }))
        .unwrap()
    )
    .await
    .is_err());
}

#[tokio::test]
async fn empty_field_declaration_reports_the_surface_contract_without_writing() {
    let node = node_with_actionrequest().await;
    let mut declaration = decl();
    declaration.fields.clear();
    let tool = BoundedWriteTool::new(ConfigAccess::local(Arc::clone(&node)), declaration)
        .declared_by(Some("ingest".into()));
    let definition = Tool::definition(&tool, String::new()).await;
    assert_eq!(definition.parameters["properties"], json!({}));
    let error = Tool::call(
        &tool,
        serde_json::from_value(json!({"summary":"queued"})).unwrap(),
    )
    .await
    .unwrap_err()
    .to_string();
    for detail in [
        "empty fields permits none",
        "Schema fields are not inherited",
        r#"surface "ingest", entry "request_action""#,
        "next request",
    ] {
        assert!(error.contains(detail), "{error}");
    }
    let rows = gents_store::config_client::ConfigAccess::local(node)
        .execute("{ ActionRequest { _docID } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["ActionRequest"], json!([]));
}

#[tokio::test]
async fn advertised_name_and_required_array_come_from_decl() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(ConfigAccess::local(node), decl());

    assert_eq!(Tool::name(&tool), "request_action");

    let def = Tool::definition(&tool, String::new()).await;
    assert_eq!(def.name, "request_action");
    assert_eq!(def.description, "Creates a new ActionRequest record; this can trigger automation watching that collection. Emit one ActionRequest.");

    let required = def.parameters["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "drift_sig"));
    assert!(required.iter().any(|v| v == "summary"));
    assert!(!required.iter().any(|v| v == "status"));

    let props = def.parameters["properties"].as_object().unwrap();
    assert!(props.contains_key("drift_sig"));
    assert!(props.contains_key("summary"));
    assert!(props.contains_key("status"));
}

#[tokio::test]
async fn runtime_backstop_rejects_invalid_graphql_identifiers() {
    let node = node_with_actionrequest().await;
    let mut empty_collection = decl();
    empty_collection.collection.clear();
    let mut bad_collection = decl();
    bad_collection.collection = "ActionRequest) { _docID } mutation {".into();
    let mut bad_field = decl();
    bad_field.fields[0].name = "drift_sig: \"injected\"".into();

    for declaration in [empty_collection, bad_collection, bad_field] {
        let result = Tool::call(
            &BoundedWriteTool::new(ConfigAccess::local(Arc::clone(&node)), declaration),
            serde_json::from_value(json!({ "drift_sig": "a", "summary": "b" })).unwrap(),
        )
        .await;
        assert!(result.is_err(), "runtime must reject invalid identifiers");
    }
}

#[tokio::test]
async fn rejects_decl_with_non_identifier_field_name() {
    let node = node_with_actionrequest().await;
    let bad = WriteToolDecl {
        notification: None,
        tool_name: "broken".into(),
        collection: "ActionRequest".into(),
        description: "field name breaks out of identifier position".into(),
        fields: vec![WriteToolField {
            name: "summary\" }) { _docID } } mutation evil { drop(input: { x".into(),
            required: false,
            fill: None,
        }],
        output_obligation: None,
    };
    let tool = BoundedWriteTool::new(ConfigAccess::local(node), bad);
    assert!(
        Tool::call(&tool, serde_json::from_value(json!({})).unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bounded_writes_follow_the_datastore_write_model() {
    const CORRELATION: &str = "run-42";
    for case in
        gents_lean_vocab::families::datastore_write_tool_call::cases::<String, String, String>()
    {
        let input = &case.input;
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_collection(&format!(
            "type {} {{ drift_sig: String summary: String status: String run_id: String expected_total: String }}",
            input.collection
        ))
        .await
        .unwrap();
        let fields = input
            .fields
            .iter()
            .map(|field| WriteToolField {
                name: field.name.clone(),
                required: field.required,
                fill: match field.fill.as_str() {
                    "caller" => None,
                    "correlation" => Some(WriteToolFieldFill::Correlation),
                    "source_field" => Some(WriteToolFieldFill::SourceField(field.name.clone())),
                    other => panic!("unknown fill {other}"),
                },
            })
            .collect::<Vec<_>>();
        let tool = BoundedWriteTool::new(
            ConfigAccess::local(Arc::clone(&node)),
            WriteToolDecl {
                notification: None,
                tool_name: "write_result".into(),
                collection: input.collection.clone(),
                description: "write a correlated result".into(),
                fields: fields.clone(),
                output_obligation: None,
            },
        )
        .declared_by(input.surface.clone());
        let definition = Tool::definition(&tool, String::new()).await;
        let properties = definition.parameters["properties"].as_object().unwrap();
        assert!(
            properties
                .keys()
                .all(|key| fields.iter().any(|field| &field.name == key)),
            "{definition:?}"
        );
        let exposed = fields
            .iter()
            .filter(|field| properties.contains_key(&field.name))
            .map(|field| field.name.clone())
            .collect();
        let arguments = input
            .supplied
            .iter()
            .map(|key| (key.clone(), json!(format!("{key}-value"))))
            .collect::<serde_json::Map<_, _>>();
        let captured = input
            .captured
            .iter()
            .map(|name| (name.clone(), format!("captured-{name}")))
            .collect();
        let result = crate::runtime_context::scope_request_tool_execution_with_trigger_context(
            None,
            tokio_util::sync::CancellationToken::new(),
            None,
            None,
            None,
            input.correlation.then(|| CORRELATION.to_string()),
            captured,
            false,
            Tool::call(&tool, super::BoundedWriteParams(arguments)),
        )
        .await;
        let selection = format!(
            "{{ {} {{ _docID {} }} }}",
            input.collection,
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let rows = node.execute(&selection).await.data.unwrap()[input.collection.as_str()]
            .as_array()
            .unwrap()
            .clone();
        assert!(rows.len() <= 1, "{rows:?}");
        let outcome = match &result {
            Ok(receipt) => {
                let receipt: serde_json::Value = serde_json::from_str(receipt).unwrap();
                let row = rows.first().expect("a written receipt names a stored row");
                assert_eq!(receipt["document_id"], row["_docID"], "{receipt}");
                for field in &fields {
                    assert_eq!(
                        receipt["document"][&field.name], row[&field.name],
                        "{receipt}"
                    );
                }
                "written"
            }
            Err(error) => {
                let text = error.to_string();
                assert!(rows.is_empty(), "{text}: {rows:?}");
                if let Some(field) = fields
                    .iter()
                    .find(|field| field.fill.is_some() && input.supplied.contains(&field.name))
                    .filter(|_| text.contains("is runtime-filled"))
                {
                    assert_eq!(
                        text,
                        gents_documents::runtime_filled_refusal(
                            "field",
                            &field.name,
                            "write_result",
                            input.surface.as_deref(),
                            field.fill.as_ref().unwrap(),
                        )
                    );
                    assert!(
                        text.contains("remove fill") && text.contains("next request"),
                        "{text}"
                    );
                    if let Some(surface) = &input.surface {
                        assert!(
                            text.contains(&format!(r#"surface "{surface}", entry "write_result""#)),
                            "{text}"
                        );
                    }
                    if field.fill == Some(WriteToolFieldFill::Correlation) {
                        assert!(text.contains("request/trigger correlation ID"), "{text}");
                    }
                    "refused_runtime_filled"
                } else if text.contains("is protected") {
                    "refused_protected"
                } else if text.contains("requires a non-empty correlation") {
                    "refused_uncorrelated"
                } else if text.contains("invalid field") {
                    "refused_invalid"
                } else {
                    panic!("{input:?}: {text}")
                }
            }
        };
        let written = rows.first().map(|row| {
            fields
                .iter()
                .map(|field| {
                    let value = &row[&field.name];
                    if value.is_null() {
                        "absent"
                    } else if *value == json!(format!("{}-value", field.name)) {
                        "caller"
                    } else if *value == json!(CORRELATION) {
                        "correlation"
                    } else if *value == json!(format!("captured-{}", field.name)) {
                        "source_field"
                    } else {
                        panic!("{input:?}: unexpected stored {value}")
                    }
                    .to_string()
                })
                .collect()
        });
        case.check(
            gents_lean_vocab::families::datastore_write_tool_call::Output {
                well_formed: tool.is_well_formed(),
                exposed,
                outcome: outcome.to_string(),
                written,
            },
        );
    }
}

#[tokio::test]
async fn native_schema_and_receipts_preserve_lists_json_and_null() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection(
        "type TypedRecord { count: Int flag: Boolean score: Float64 labels: [String] data: JSON }",
    )
    .await
    .unwrap();
    let tool = BoundedWriteTool::new(
        ConfigAccess::local(node.clone()),
        WriteToolDecl {
            notification: None,
            tool_name: "record_typed".into(),
            collection: "TypedRecord".into(),
            description: "Record typed data".into(),
            output_obligation: None,
            fields: ["count", "flag", "score", "labels", "data"]
                .into_iter()
                .map(|name| WriteToolField {
                    name: name.into(),
                    required: true,
                    fill: None,
                })
                .collect(),
        },
    );
    let definition = Tool::definition(&tool, String::new()).await;
    assert_eq!(
        definition.parameters["properties"]["count"]["type"],
        json!(["integer", "null"])
    );
    assert_eq!(
        definition.parameters["properties"]["flag"]["type"],
        json!(["boolean", "null"])
    );
    let args = json!({"count":86,"flag":false,"score":0.5,"labels":["quoted\"\nlabel"],"data":{"measured":true,"readings":[1,2]}});
    let receipt = Tool::call(&tool, serde_json::from_value(args.clone()).unwrap())
        .await
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    for (field, value) in args.as_object().unwrap() {
        assert_eq!(&receipt["document"][field], value, "{field}");
    }
    let args = json!({"count":null,"flag":null,"score":null,"labels":[],"data":null});
    let receipt = Tool::call(&tool, serde_json::from_value(args).unwrap())
        .await
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert!(receipt["document"]["count"].is_null());
    assert!(receipt["document"]["labels"].is_null());
    assert!(super::input::literal("JSON", &json!({"bad-key":1})).is_err());
    assert!(super::input::literal("JSON", &json!([])).is_err());
    assert!(super::input::literal("JSON", &json!({"nested": [1, []]})).is_err());
}

#[tokio::test]
async fn count_field_type_decides_whether_a_refused_obligation_can_register() {
    use gents_documents::{WriteToolOutputObligation, WriteToolOutputObligationScope};

    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection(
        r#"
        type CountOwner {
            flag: Boolean
            labels: [String]
            tallies: [CountTally]
        }
        type CountTally {
            label: String
            owner: CountOwner @primary
        }
    "#,
    )
    .await
    .unwrap();

    let tool_for = |collection: &str, field: &str| {
        BoundedWriteTool::new(
            ConfigAccess::local(Arc::clone(&node)),
            WriteToolDecl {
                notification: None,
                tool_name: "record_tally".into(),
                collection: collection.into(),
                description: "Record one tally.".into(),
                fields: vec![WriteToolField {
                    name: field.into(),
                    required: true,
                    fill: None,
                }],
                output_obligation: Some(WriteToolOutputObligation {
                    scope: WriteToolOutputObligationScope::Request,
                    minimum_writes: 1,
                    expected_count_field: Some(field.into()),
                }),
            },
        )
    };

    for field in ["flag", "labels"] {
        let tool = tool_for("CountOwner", field);
        let schema = tool.field_types().unwrap()[field].clone();
        assert!(
            !gents_store::config_client::schema_introspection::can_hold_canonical_count(&schema),
            "`{field}` is `{schema}`, which publication refuses"
        );
        assert!(
            tool.is_well_formed(),
            "`{field}` is `{schema}`, which resolves an argument schema"
        );
    }

    for (collection, field) in [("CountTally", "owner"), ("CountOwner", "tallies")] {
        let tool = tool_for(collection, field);
        assert!(
            !tool.is_well_formed(),
            "relation-typed `{collection}.{field}` resolves no argument schema"
        );
        let error = tool.ensure_well_formed().unwrap_err();
        assert!(
            format!("{error:#}").contains("unsupported bounded write field type `Object`"),
            "{error:#}"
        );
    }

    let absent = tool_for("CountOwner", "missing_total");
    assert!(!absent.is_well_formed());
    let error = absent.ensure_well_formed().unwrap_err();
    assert!(
        format!("{error:#}").contains("field `missing_total` is absent from `CountOwner`"),
        "{error:#}"
    );
}

/// Premise evidence for the ledger row `boundary.count-carrier.non-null-id`.
#[tokio::test]
async fn defradb_refuses_a_non_null_id_count_carrier() {
    let node = gents_store::test_support::node_builder()
        .build()
        .await
        .unwrap();
    let error = node
        .add_collection("type CountCarrierNonNullId { count: ID! }")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("NonNull variant for type ID is not supported"),
        "{error:#}"
    );
}

#[tokio::test]
async fn count_carriers_store_as_the_count_carrier_model() {
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    let maximum = gents_documents::automation::MAX_EVENT_TRIGGER_GROUP_DOCS;
    for (index, case) in gents_lean_vocab::families::count_carrier_storage::cases()
        .into_iter()
        .enumerate()
    {
        let schema = case.input.schema.as_str();
        let collection = format!("CountCarrier{index}");
        node.add_collection(&format!("type {collection} {{ count: {schema} }}"))
            .await
            .unwrap();
        let declaration = WriteToolDecl {
            notification: None,
            tool_name: format!("write_count_{index}"),
            collection: collection.clone(),
            description: "Persist a count carrier".into(),
            fields: vec![WriteToolField {
                name: "count".into(),
                required: true,
                fill: None,
            }],
            output_obligation: None,
        };
        let tool = BoundedWriteTool::new(
            ConfigAccess::local_as(
                node.clone(),
                identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
            ),
            declaration,
        );
        let input: serde_json::Value = match case.input.probe.kind.as_str() {
            "text" => serde_json::Value::String(case.input.probe.spelling.clone()),
            _ => serde_json::from_str(&case.input.probe.spelling).unwrap(),
        };
        Tool::call(
            &tool,
            serde_json::from_value(json!({"count": input})).unwrap(),
        )
        .await
        .unwrap_or_else(|error| panic!("{schema}: {error:#}"));
        let query = format!("{{ {collection} {{ count }} }}");
        let response = gents_store::graphql::graphql_with_transaction_retry(
            &node,
            &query,
            "read count carrier",
        )
        .await
        .unwrap();
        assert!(!response.has_errors(), "{schema}: {:?}", response.errors);
        let output = response.data.as_ref().unwrap()[&collection][0]["count"].clone();
        let introspection = gents_store::graphql::graphql_with_transaction_retry(
            &node,
            &gents_store::config_client::schema_introspection::introspection_query(&collection)
                .unwrap(),
            "introspect count carrier",
        )
        .await
        .unwrap();
        let decoded = gents_store::config_client::schema_introspection::parse_collection_schema(
            introspection.data.as_ref(),
        )
        .unwrap();
        let field = decoded
            .fields
            .iter()
            .find(|field| field.name == "count")
            .unwrap();
        let introspected =
            gents_store::config_client::schema_introspection::can_hold_canonical_count(
                field.named_type(),
            );
        let decimal_admitted = if introspected {
            None
        } else {
            Some(
                Tool::call(
                    &tool,
                    serde_json::from_value(json!({"count":"10"})).unwrap(),
                )
                .await
                .is_ok(),
            )
        };
        case.check(
            gents_lean_vocab::families::count_carrier_storage::StorageOutput {
                argument: gents_store::graphql::canonical_positive_count(&input, maximum)
                    .map(|count| count as u64),
                stored: gents_store::graphql::canonical_positive_count(&output, maximum)
                    .map(|count| count as u64),
                introspected,
                decimal_admitted,
            },
        );
    }
}

#[test]
fn write_grant_guidance_names_the_call_that_keeps_the_callers_datastore_group() {
    let group: gents_documents::DatastoreTools = serde_json::from_value(json!({
        "enable_defra_query": true,
        "write_collections": ["Kept"],
    }))
    .unwrap();
    let names = ["Board".to_owned()];
    let held = super::write_grant_guidance(true, Some(&group), &names);
    assert_eq!(held["writable"], false);
    assert_eq!(held["next_call"]["tool"], "config");
    assert_eq!(held["next_call"]["args"]["argv"], json!(["tools", "edit"]));
    let set = &held["next_call"]["args"]["set"]["datastore"];
    assert_eq!(set["write_collections"], json!(["Kept", "Board"]));
    assert_eq!(
        set["enable_defra_query"], true,
        "the group is sent back whole"
    );

    let unheld = super::write_grant_guidance(false, Some(&group), &names);
    assert!(unheld.get("next_call").is_none(), "{unheld}");
    assert!(unheld["how"]
        .as_str()
        .unwrap()
        .contains("you do not hold the tools category"));

    let granted = super::write_grant_guidance(false, Some(&group), &["Kept".to_owned()]);
    assert_eq!(granted["writable"], true);
}
