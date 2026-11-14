pub(crate) use gents_pack::pack::authoring::export::write_manifest_root;

#[cfg(test)]
mod tests {
    use super::super::DesiredStateManifest;
    use super::*;

    #[test]
    fn dollar_text_round_trips_through_export_and_load() {
        let script = "echo ${HOME} $${X} $$ $";
        let prompt = "context ${HOME} $${X} $$";
        let config: DesiredStateManifest = serde_json::from_value(serde_json::json!({
            "node":{"node_did":"owner"},
            "contexts":[{"node_did":"owner","context_id":"context","system_prompt":prompt}],
            "agents":[{"node_did":"owner","agent_did":"agent","creation_nonce":"nonce","context_id":"context",
                "inference_profile_id":"profile"}],
            "tasks":[{"node_did":"owner","task_id":"task","agent_did":"agent",
                "prompt_template":"work","hooks":[
                    {"hook_id":"gate","phase":"before","command":["sh","-c",script]}
            ]}]
        }))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_manifest_root(dir.path(), &config, false).unwrap();
        let (loaded, report) = super::super::load_manifest_root(dir.path());
        assert!(report.ok, "{:?}", report.errors);
        let loaded = loaded.unwrap();
        assert_eq!(loaded.tasks[0].hooks[0].command[2], script);
        assert_eq!(loaded.contexts[0].system_prompt.as_deref(), Some(prompt));
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
    }
}
