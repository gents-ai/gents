use gents_lean_vocab::lean_plugin_host_import_cases;

/// Lean `hostImportCases`: a plugin declaring `run_script` has the `gents`
/// imports linked, and the import answers `result` for a call with a script
/// and `no_script` for one without; an undeclared plugin has none.
#[test]
fn generated_plugin_host_import_cases_drive_the_grant() {
    use crate::pack::{HostImport, PackPlugin};
    let cases = lean_plugin_host_import_cases();
    assert_eq!(cases.len(), 4, "Lean host-import rows drifted");
    for (index, row) in cases.iter().enumerate() {
        let plugin = PackPlugin {
            name: "workflow".into(),
            description: "d".into(),
            artifact: "plugins/workflow.afb".into(),
            language: "rust".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"script": {"type": "string"}}}),
            job: gents_protocol::job::PluginJobDeclaration(true),
            host_imports: if row.declared {
                vec![HostImport::RunScript]
            } else {
                Vec::new()
            },
            ..PackPlugin::default()
        };
        plugin
            .validate()
            .unwrap_or_else(|e| panic!("host_import {index}: {e}"));
        let linked = !plugin.host_imports.is_empty();
        assert_eq!(linked, row.linked, "host_import {index}: linked");
        let arguments = if row.call_has_script {
            serde_json::json!({"script": "module.exports = (i) => i"})
        } else {
            serde_json::json!({})
        };
        let answer = linked.then(|| {
            if crate::plugin::host_imports::script_of(&arguments).is_some() {
                "result"
            } else {
                "no_script"
            }
        });
        assert_eq!(answer, row.answer.as_deref(), "host_import {index}: answer");
    }
}
