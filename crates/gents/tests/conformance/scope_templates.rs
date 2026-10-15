use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::templates::{
    admit_app_collections, builtin_templates, equality_filter, filter_conditions, resolve_template,
    scope_filter, single_string_eq, Delivery, FilterPredicate, Scope, ScopeTemplate,
    NODE_DIRECTORY_COLLECTION,
};
use gents::agent::p2p_reconcile::{
    client_route_collections, resolve_template_filters, PairingDirection, CLIENT_COLLECTIONS,
    CLIENT_TEMPLATE, CLIENT_TO_RUNTIME_COLLECTIONS,
};

const LEAN_SCOPE_STATE: &str = include_str!("../../proofs/Proofs/ScopeTemplates/State.lean");

fn lean_string_list(definition: &str) -> Vec<String> {
    let marker = format!("def {definition} : List String :=");
    let body = LEAN_SCOPE_STATE
        .split_once(&marker)
        .unwrap_or_else(|| panic!("Lean scope model omitted {definition}"))
        .1
        .split("\n\n")
        .next()
        .expect("Lean list body");
    body.lines()
        .flat_map(|line| {
            line.split('"')
                .enumerate()
                .filter_map(|(index, value)| (index % 2 == 1).then_some(value.to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A `CollectionRule` of the Lean scope model: `(collection, field, source)`.
fn lean_collection_rules(definition: &str) -> Vec<(String, String, String)> {
    crate::lean_vocab_test::lean_contract_snapshot()
        .scope_collection_rules
        .get(definition)
        .unwrap_or_else(|| panic!("Lean scope model omitted {definition}"))
        .iter()
        .map(|rule| {
            (
                rule.collection.clone(),
                rule.field.clone(),
                rule.source.clone(),
            )
        })
        .collect()
}

/// Whether the route the real template owner resolves for `peer_did` selects
/// the stored `AgentRequest` row `request_doc_id`. DefraDB evaluates the
/// resolved filter, as it does for the replicator the route installs.
async fn route_selects_request(
    node: &gents::defra_node::EmbeddedNode,
    template: &ScopeTemplate,
    request_doc_id: &str,
    peer_did: &str,
    local_did: &str,
) -> bool {
    let filters = scope_filter(&template.scope, template.collections, peer_did, local_did);
    let Some(predicate) = filters.get("AgentRequest") else {
        return false;
    };
    let conditions = filter_conditions(predicate).expect("route filter conditions");
    let filter =
        gents_protocol::graphql::graphql_input_literal(&serde_json::Value::Object(conditions))
            .expect("render route filter");
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _and: [{{ _docID: {{ _eq: "{}" }} }}, {filter}] }}) {{ _docID }} }}"#,
            gents::graphql::escape_graphql_string(request_doc_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response
        .data
        .as_ref()
        .and_then(|data| data["AgentRequest"].as_array())
        .is_some_and(|rows| !rows.is_empty())
}

/// Store one `AgentRequest` whose Lean-named route fields hold the given DIDs.
async fn store_request_row(
    node: &gents::defra_node::EmbeddedNode,
    fields: &[(&str, &str)],
) -> String {
    let input = fields
        .iter()
        .map(|(field, value)| {
            format!(
                r#"{field}: "{}""#,
                gents::graphql::escape_graphql_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let response = node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "wave-request", {input} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    gents::graphql::single_mutation_document(&response, "create_AgentRequest")
        .expect("stored request mutation")
        .and_then(|document| document["_docID"].as_str())
        .expect("stored request identity")
        .to_owned()
}

fn assert_eq_filter(predicate: &FilterPredicate, field: &str, value: &str) {
    assert_eq!(single_string_eq(predicate), Some((field, value)));
}

fn assert_and_eq_filter(predicate: &FilterPredicate, expected: &[(&str, &str)]) {
    let conditions = filter_conditions(predicate).expect("predicate filter");
    let clauses = conditions
        .get("_and")
        .and_then(serde_json::Value::as_array)
        .expect("conjunctive _and filter");
    assert_eq!(clauses.len(), expected.len());
    for (field, value) in expected {
        assert!(
            clauses.iter().any(|clause| {
                clause
                    .get(*field)
                    .and_then(|operators| operators.get("_eq"))
                    .and_then(serde_json::Value::as_str)
                    == Some(*value)
            }),
            "missing {field} = {value:?} in {conditions:?}"
        );
    }
}

#[test]
fn resolve_template_is_total_over_catalog_and_id_faithful() {
    assert_eq!(
        builtin_templates()
            .iter()
            .map(|template| template.id)
            .collect::<Vec<_>>(),
        vec![
            "conversation",
            "machine",
            "client",
            "agent-config",
            "backup",
            "agent-target-caller",
            "agent-target-host",
            "app-collections",
            "client-index",
        ],
        "the Rust catalog must remain identical to Lean builtinCatalog"
    );
    for t in builtin_templates() {
        let resolved = resolve_template(t.id).expect("catalog id resolves");
        assert_eq!(resolved.id, t.id, "resolution must not alias ids");
    }
    assert!(
        resolve_template("definitely-not-a-template").is_none(),
        "unknown id resolves to none"
    );
}

/// Mirrors Lean `client_request_filter_conjoins_requester_and_destination`,
/// `client_transcript_destination_scoped`, and the exact directional
/// collection theorems.
#[test]
fn client_route_is_directional_destination_scoped_and_control_plane_bounded() {
    let transcript_names = lean_string_list("transcriptCollections")
        .into_iter()
        .chain(lean_string_list("clientTranscriptCollections"))
        .collect::<Vec<_>>();
    let transcript = transcript_names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let transcript = transcript.as_slice();
    let control_names = lean_string_list("clientToRuntimeCollections");
    let client_to_runtime = transcript
        .iter()
        .copied()
        .chain(control_names.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let client_to_runtime = client_to_runtime.as_slice();
    const RETURN_CONTROL_PLANE: &[&str] = &[
        "Agent",
        "AgentContext",
        "CompactionConfig",
        "Tools",
        "AgentTarget",
        "InferenceProfile",
        "InferenceSampling",
        "InferenceExecution",
        "InferenceRetryPolicy",
        "ToolServiceRegistry",
        "Skill",
        "DatastoreToolSurface",
        "ChainKeyBinding",
        "EthTool",
        "Task",
        "Schedule",
        "Trigger",
        "EventSource",
    ];
    const OWNER_PROJECTION: &[&str] = &["NodeReadiness"];
    let runtime_to_client = client_to_runtime
        .iter()
        .copied()
        .chain(RETURN_CONTROL_PLANE.iter().copied())
        .chain(OWNER_PROJECTION.iter().copied())
        .collect::<Vec<_>>();
    let runtime_to_client = runtime_to_client.as_slice();

    assert_eq!(
        lean_string_list("clientOwnerProjectionCollections"),
        OWNER_PROJECTION,
        "Rust owner-projection policy must conform to the checked Lean model source"
    );

    for collection in RETURN_CONTROL_PLANE {
        assert!(
            gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(collection),
            "canonical return collection {collection} must be installed in the runtime schema"
        );
    }

    let template = resolve_template(CLIENT_TEMPLATE).expect("client in catalog");
    assert_eq!(template.delivery, Delivery::Push);
    assert!(matches!(template.scope, Scope::ClientRoute));
    assert_eq!(CLIENT_TO_RUNTIME_COLLECTIONS, client_to_runtime);
    assert_eq!(CLIENT_COLLECTIONS, runtime_to_client);
    assert_eq!(template.collections, runtime_to_client);
    assert_eq!(
        client_route_collections(PairingDirection::ClientToRuntime),
        client_to_runtime
    );
    assert_eq!(
        client_route_collections(PairingDirection::RuntimeToClient),
        runtime_to_client
    );

    let requester = "did:key:phone";
    let owner = "did:key:mandrake";
    let non_owner = "did:key:amy";
    let outbound = resolve_template_filters(
        template,
        PairingDirection::ClientToRuntime,
        requester,
        owner,
    );
    assert_eq!(outbound.len(), client_to_runtime.len());
    for collection in transcript {
        assert_and_eq_filter(
            outbound.get(*collection).expect("transcript filter"),
            &[("requester_did", requester), ("node_did", owner)],
        );
        let encoded = serde_json::to_string(outbound.get(*collection).unwrap()).unwrap();
        assert!(
            !encoded.contains(non_owner),
            "{collection} must not admit the non-owning destination"
        );
    }
    assert_eq_filter(
        outbound
            .get("PeerEndpoint")
            .expect("outbound endpoint filter"),
        "did",
        requester,
    );
    assert_and_eq_filter(
        outbound
            .get("SessionHydrationRequest")
            .expect("hydration request filter"),
        &[("requester_did", requester), ("node_did", owner)],
    );
    assert_and_eq_filter(
        outbound
            .get("AgentSessionInputEdit")
            .expect("signed input edit request filter"),
        &[("requester_did", requester), ("node_did", owner)],
    );
    for collection in RETURN_CONTROL_PLANE {
        assert!(!outbound.contains_key(*collection));
        assert!(!client_to_runtime.contains(collection));
    }

    let returning = resolve_template_filters(
        template,
        PairingDirection::RuntimeToClient,
        requester,
        owner,
    );
    assert_and_eq_filter(
        returning
            .get("AgentSessionInputEdit")
            .expect("signed input edit receipt filter"),
        &[("requester_did", requester), ("node_did", owner)],
    );
    assert_eq!(
        returning.len(),
        client_to_runtime.len() + OWNER_PROJECTION.len()
    );
    assert_eq_filter(
        returning
            .get("PeerEndpoint")
            .expect("return endpoint filter"),
        "did",
        owner,
    );
    for collection in RETURN_CONTROL_PLANE {
        assert!(template.collections.contains(collection));
        assert!(
            !returning.contains_key(*collection),
            "bounded return control-plane collection {collection} is deliberately unfiltered"
        );
    }
    assert_eq_filter(
        returning
            .get("NodeReadiness")
            .expect("readiness owner filter"),
        "node_did",
        owner,
    );
    assert!(!outbound.contains_key("NodeReadiness"));
    for excluded in gents_protocol::schemas::CREDENTIAL_COLLECTION_NAMES
        .iter()
        .chain(&["PeerPairingDesired", "DataPlanePairingDesired"])
    {
        assert!(
            !template.collections.contains(excluded),
            "client route must exclude {excluded}"
        );
    }
}

#[test]
fn sensitive_local_audit_payloads_have_no_builtin_replication_route() {
    for template in builtin_templates() {
        for collection in ["RenderedRequest", "ProviderContextReduction"] {
            assert!(
                !template.collections.contains(&collection),
                "{} must not replicate local audit collection {collection}",
                template.id
            );
        }
    }
}

#[test]
fn conversation_scope_filters_transcript_and_grants_unfiltered_config() {
    let t = resolve_template("conversation").expect("conversation in catalog");
    assert_eq!(t.delivery, Delivery::Push);
    assert!(matches!(t.scope, Scope::PerCollection(_)));

    let filter = scope_filter(&t.scope, t.collections, "did:key:bob", "did:key:alice");
    for col in lean_string_list("transcriptCollections") {
        let pred = filter
            .get(col.as_str())
            .expect("transcript collection filter");
        assert_eq_filter(pred, "requester_did", "did:key:bob");
    }
    for col in [
        "Agent",
        "AgentContext",
        "CompactionConfig",
        "Tools",
        "AgentTarget",
        "InferenceProfile",
        "InferenceSampling",
        "InferenceExecution",
        "InferenceRetryPolicy",
        "ToolServiceRegistry",
        "Skill",
        "DatastoreToolSurface",
        "ChainKeyBinding",
        "EthTool",
    ] {
        assert!(t.collections.contains(&col));
        assert!(!filter.contains_key(col), "config {col} must be unfiltered");
    }
}

#[test]
fn conversation_scope_excludes_another_requester_on_the_same_agent() {
    let t = resolve_template("conversation").expect("conversation in catalog");
    let phone_filter = scope_filter(&t.scope, t.collections, "did:key:phone", "did:key:amy");
    let predicate = phone_filter
        .get("AgentRequest")
        .expect("request filter is present");

    assert_eq_filter(predicate, "requester_did", "did:key:phone");
}

/// Mirrors Lean `clientIndex_filter_eq` and
/// `clientIndex_filters_requester_lineage`.
#[test]
fn client_index_scope_is_exactly_the_requester_scoped_literal_index() {
    let template = resolve_template("client-index").expect("client-index in catalog");
    assert_eq!(template.delivery, Delivery::Push);
    assert_eq!(template.collections, &["AgentSession", "MailboxItem"]);

    let phone = scope_filter(
        &template.scope,
        template.collections,
        "did:key:phone",
        "did:key:home",
    );
    assert_eq!(phone.len(), 2);
    for collection in template.collections {
        let predicate = phone.get(*collection).expect("collection filtered");
        assert_eq_filter(predicate, "requester_did", "did:key:phone");
    }

    let laptop = scope_filter(
        &template.scope,
        template.collections,
        "did:key:laptop",
        "did:key:home",
    );
    assert_ne!(
        phone.get("AgentSession").unwrap(),
        laptop.get("AgentSession").unwrap()
    );
}

#[test]
fn machine_filters_transcript_and_directory() {
    let template = resolve_template("machine").expect("machine in catalog");
    let filters = scope_filter(
        &template.scope,
        template.collections,
        "did:key:phone",
        "did:key:issuer",
    );

    for collection in lean_string_list("transcriptCollections") {
        let predicate = filters
            .get(collection.as_str())
            .expect("conversation filter");
        assert_eq_filter(predicate, "requester_did", "did:key:phone");
    }
    assert_eq!(
        filters.get(NODE_DIRECTORY_COLLECTION),
        Some(&equality_filter("source_did", "did:key:issuer"))
    );
}

#[test]
fn unscoped_scope_resolves_to_no_filter() {
    for id in ["agent-config", "backup"] {
        let t = resolve_template(id).expect("template in catalog");
        assert!(matches!(t.scope, Scope::Unscoped));
        assert!(
            scope_filter(&t.scope, t.collections, "did:key:bob", "did:key:alice").is_empty(),
            "{id} must be unfiltered"
        );
    }
}

#[test]
fn agent_target_templates_resolve_to_exact_directional_filters() {
    let caller = "did:key:coord";
    let host_did = "did:key:host";
    for (template_id, collections_def, rules_def, peer_did, local_did) in [
        (
            "agent-target-caller",
            "agentTargetCallerCollections",
            "agentTargetCallerRules",
            host_did,
            caller,
        ),
        (
            "agent-target-host",
            "agentTargetHostCollections",
            "agentTargetHostRules",
            caller,
            host_did,
        ),
    ] {
        let collections = lean_string_list(collections_def);
        let rules = lean_collection_rules(rules_def);
        let template = resolve_template(template_id).expect("agent-target template");
        assert_eq!(template.delivery, Delivery::Push);
        assert_eq!(
            template.collections,
            collections.iter().map(String::as_str).collect::<Vec<_>>(),
            "{template_id} collections must equal Lean {collections_def}"
        );
        assert_eq!(
            rules.iter().map(|(c, _, _)| c).collect::<Vec<_>>(),
            collections.iter().collect::<Vec<_>>(),
            "Lean {rules_def} must scope every declared collection"
        );
        let filter = scope_filter(&template.scope, template.collections, peer_did, local_did);
        assert_eq!(filter.len(), rules.len());
        for (collection, field, source) in &rules {
            assert_eq!(
                source, "peerDid",
                "{template_id} {collection} is peer scoped"
            );
            assert_eq!(
                filter.get(collection.as_str()),
                Some(&equality_filter(field.as_str(), peer_did)),
                "unexpected {template_id} filter for {collection}"
            );
        }
        assert!(!template.collections.contains(&"AgentToolCall"));
        assert!(!filter.contains_key("AgentToolCall"));
    }

    let coordinator_rules = lean_collection_rules("agentTargetCallerRules");
    assert_eq!(
        coordinator_rules
            .iter()
            .map(|(c, f, _)| (c.as_str(), f.as_str()))
            .collect::<Vec<_>>(),
        [("AgentRequest", "node_did")],
        "caller -> host carries exactly the Peer AgentRequest, scoped by its target"
    );
    let host_rules = lean_collection_rules("agentTargetHostRules");
    assert!(
        host_rules
            .iter()
            .all(|(_, field, _)| field == "requester_did"),
        "host -> caller returns the caused request lineage by requester_did"
    );
    for collection in [
        "AgentRequest",
        "AgentSession",
        "AgentOutputSegment",
        "AgentMessage",
    ] {
        assert!(host_rules.iter().any(|(c, _, _)| c == collection));
    }
}

#[test]
fn agent_target_host_message_filter_excludes_unrelated_host_history() {
    let host = resolve_template("agent-target-host").expect("host template");
    let filter = scope_filter(
        &host.scope,
        host.collections,
        "did:key:coord",
        "did:key:host",
    );
    let predicate = filter.get("AgentMessage").expect("message filter");

    assert_eq_filter(predicate, "requester_did", "did:key:coord");
}

#[tokio::test]
async fn sixteen_peer_request_wave_is_reduced_to_one_target() {
    let request_field = |rules_def: &str| {
        lean_collection_rules(rules_def)
            .into_iter()
            .find(|(collection, _, _)| collection == "AgentRequest")
            .map(|(_, field, _)| field)
            .unwrap_or_else(|| panic!("Lean {rules_def} carries AgentRequest"))
    };
    let target_field = request_field("agentTargetCallerRules");
    let requester_field = request_field("agentTargetHostRules");
    let caller = "did:key:coordinator-07";
    let target = "did:key:host-07";

    let node = gents::defra_node::EmbeddedNode::builder()
        .build()
        .await
        .expect("route evaluation node");
    node.add_schema(gents_protocol::schemas::AGENT_REQUEST)
        .await
        .expect("AgentRequest schema");
    let request = store_request_row(
        &node,
        &[(&target_field, target), (&requester_field, caller)],
    )
    .await;

    let coordinator = resolve_template("agent-target-caller").expect("coordinator template");
    let mut routed_to = Vec::new();
    for index in 0..16 {
        let host_did = format!("did:key:host-{index:02}");
        if route_selects_request(&node, coordinator, &request, &host_did, caller).await {
            routed_to.push(host_did);
        }
    }
    assert_eq!(routed_to, [target]);

    let host = resolve_template("agent-target-host").expect("host template");
    let mut returned_to = Vec::new();
    for index in 0..16 {
        let peer_did = format!("did:key:coordinator-{index:02}");
        if route_selects_request(&node, host, &request, &peer_did, target).await {
            returned_to.push(peer_did);
        }
    }
    assert_eq!(returned_to, [caller]);
    node.shutdown().await;
}

#[test]
fn app_collections_template_is_unscoped_replicate_byo() {
    let t = resolve_template("app-collections").expect("app-collections in catalog");
    assert_eq!(t.id, "app-collections");
    assert!(matches!(t.delivery, Delivery::Replicate));
    assert!(matches!(t.scope, Scope::Unscoped));
    assert!(
        t.collections.is_empty(),
        "app-collections carries no fixed collections; the row supplies them"
    );
    let f = scope_filter(
        &t.scope,
        &["ChangeProposed"],
        "did:key:bob",
        "did:key:alice",
    );
    assert!(f.is_empty(), "unscoped app-collections must not filter");
}

#[test]
fn app_collection_admission_matches_lean_protocol_disjointness_contract() {
    assert!(admit_app_collections(BTreeSet::new()).is_none());

    let custom = BTreeSet::from(["ChangeProposed".to_string()]);
    assert_eq!(admit_app_collections(custom.clone()), Some(custom));

    for protocol in gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
    {
        let requested = BTreeSet::from(["ChangeProposed".to_string(), (*protocol).to_string()]);
        assert!(
            admit_app_collections(requested).is_none(),
            "Lean appCollections_protocol_overlap_rejected violated by {protocol}"
        );
    }
}

/// Template selection is not ACP authorization. Ordinary routes may never
/// select credentials; the explicit operator route still needs DID/ACP admission.
/// The Rust credential set that broad subscriptions exclude is Lean's.
#[test]
fn ordinary_routes_exclude_credentials_and_operator_selection_remains_explicit() {
    assert_eq!(
        lean_string_list("credentialCollections"),
        gents_protocol::schemas::CREDENTIAL_COLLECTION_NAMES,
        "Rust credential collections must conform to the checked Lean model source"
    );
    for id in [
        "client",
        "conversation",
        "machine",
        "client-index",
        "agent-target-host",
        "agent-target-caller",
    ] {
        let template = resolve_template(id).expect("builtin route");
        for credential in gents_protocol::schemas::CREDENTIAL_COLLECTION_NAMES {
            assert!(
                !template.collections.contains(credential),
                "{id} leaks {credential}"
            );
        }
    }
    assert!(resolve_template("agent-config")
        .unwrap()
        .collections
        .contains(&"InferenceBackend"));
}
