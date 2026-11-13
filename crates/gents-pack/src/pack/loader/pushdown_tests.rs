use crate::test_support::load_pack_system_prompt;

#[test]
fn lsp_pack_prompt_follows_agent_context_sidecar_reference() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("README.md"), "LSP prompt fixture\n").unwrap();
    std::fs::create_dir_all(root.path().join("custom_assets")).unwrap();
    std::fs::write(
        root.path().join("custom_assets/prompt.md"),
        "Exact prompt\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "manifest_version":1,"name":"lsp_test","version":"1",
            "description":"Fixture","kind":"documents","authors":["Test"],
            "assets":["README.md","pack_config.json","custom_assets/prompt.md"],
            "config":"pack_config.json",
            "inference_slots":[{"name":"coder","description":"Fixture coder","agents":["lsp-coder"]}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.path().join("pack_config.json"),
        serde_json::to_vec(&serde_json::json!({
            "node":{},
            "agents":[{"agent_did":"lsp-coder","creation_nonce":"lsp-coder-nonce","context_id":"distinct-context","inference_profile_id":"gents:inference-slot:coder"}],
            "contexts":[{"context_id":"distinct-context","system_prompt":"./custom_assets/prompt.md"}]
        })).unwrap(),
    ).unwrap();
    assert_eq!(
        load_pack_system_prompt(root.path()).unwrap(),
        "Exact prompt\n"
    );
}
