use std::collections::BTreeSet;

#[test]
fn runtime_data_has_no_replication_template_or_app_escape() {
    let contracts = gents_lean_vocab::lean_identity_contracts();
    let allowed = &contracts[0].replicated_protocol_collections;
    assert_eq!(
        crate::schemas::REPLICATED_PROTOCOL_COLLECTION_NAMES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        allowed.iter().map(String::as_str).collect()
    );
    for collection in query::parse_sdl(&crate::schemas::ALL.join("\n")).unwrap() {
        assert_eq!(
            collection.is_branchable,
            allowed.contains(&collection.name),
            "{} locality",
            collection.name
        );
    }
    for collection in crate::schemas::ALL_COLLECTION_NAMES {
        assert_eq!(
            crate::schemas::is_replicated_protocol_collection(collection),
            allowed.iter().any(|name| name == collection)
        );
    }
}
