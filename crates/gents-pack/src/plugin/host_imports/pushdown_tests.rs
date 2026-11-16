#[test]
fn host_imports_follow_the_lean_model() {
    use crate::pack::{HostImport, PackPlugin};
    gents_lean_vocab::families::plugin_resources_host_import::replay::<String>(|i| {
        let plugin = PackPlugin {
            name: "workflow".into(),
            description: "d".into(),
            artifact: "plugins/workflow.afb".into(),
            language: "rust".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"script": {"type": "string"}}}),
            job: gents_protocol::job::PluginJobDeclaration(true),
            host_imports: if i.declared {
                vec![HostImport::RunScript]
            } else {
                Vec::new()
            },
            ..PackPlugin::default()
        };
        plugin.validate().unwrap_or_else(|e| panic!("{i:?}: {e}"));
        let linked = !plugin.host_imports.is_empty();
        let arguments = if i.call_has_script {
            serde_json::json!({"script": "module.exports = (i) => i"})
        } else {
            serde_json::json!({})
        };
        let answer = linked.then(|| {
            if crate::plugin::host_imports::script_of(&arguments).is_some() {
                "result".to_owned()
            } else {
                "no_script".to_owned()
            }
        });
        gents_lean_vocab::families::plugin_resources_host_import::HostImportObservation {
            linked,
            answer,
        }
    });
}
