#[test]
fn runtime_data_has_no_replication_template_or_app_escape() {
    use gents_lean_vocab::families::agent_identity_collection_locality as locality;
    let cases = gents_lean_vocab::families::agent_identity_collection_locality::cases();
    let collections = query::parse_sdl(&crate::schemas::ALL.join("\n")).unwrap();
    for case in &cases {
        let name = case.input.collection.as_str();
        let replicates = crate::schemas::is_replicated_protocol_collection(name);
        assert_eq!(
            crate::schemas::REPLICATED_PROTOCOL_COLLECTION_NAMES.contains(&name),
            replicates,
            "{name}"
        );
        assert_eq!(
            crate::schemas::is_local_only_collection(name),
            !replicates,
            "{name}"
        );
        if let Some(collection) = collections.iter().find(|collection| collection.name == name) {
            assert_eq!(collection.is_branchable, replicates, "{name} locality");
        }
        case.check(locality::LocalityOutput { replicates });
    }
    for collection in &collections {
        assert_eq!(
            collection.is_branchable,
            crate::schemas::is_replicated_protocol_collection(&collection.name),
            "{} locality",
            collection.name
        );
    }
    for name in crate::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(crate::schemas::REPLICATED_PROTOCOL_COLLECTION_NAMES.iter())
        .filter(|name| crate::schemas::is_replicated_protocol_collection(name))
    {
        assert!(
            cases.iter().any(|case| case.input.collection == *name),
            "{name} replicates without a modeled protocol collection"
        );
    }
}
