//! The shared executor and the model-tool adapter over it.

use std::sync::Arc;

use super::{build_plugin_pack, ECHO_WAT};
use crate::document_config::PluginToolRef;
use crate::llm::tool::ToolDyn;
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store::{self, InstalledPlugin};
use crate::plugin::tool::PluginTool;

/// Installs the echo plugin as `team/plugin` under a fresh home.
fn installed_echo() -> (tempfile::TempDir, InstalledPlugin) {
    let (declaration, bytes) = build_plugin_pack("echo_pack", ECHO_WAT, None);
    let home = tempfile::tempdir().unwrap();
    let hex = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes));
    store::store_bytes(home.path(), &hex, &bytes).unwrap();
    let record = InstalledPlugin {
        namespace: "team".into(),
        name: declaration.name.clone(),
        version: "0.1.0".into(),
        digest: format!("sha256:{hex}"),
        language: "rust".into(),
        declaration,
        granted: None,
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

fn tool_ref(digest: Option<&str>) -> PluginToolRef {
    PluginToolRef {
        plugin: "team/plugin".into(),
        digest: digest.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_model_tool_runs_the_installed_plugin_with_its_declared_schema() {
    let (home, record) = installed_echo();
    let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
    let tool = PluginTool::resolve(executor.clone(), &tool_ref(Some(&record.digest))).unwrap();

    let definition = tool.definition(String::new()).await;
    assert_eq!(definition.name, "plugin");
    assert_eq!(definition.description, record.declaration.description);
    assert_eq!(definition.parameters, record.declaration.input_schema);

    for n in 0..2 {
        let output = tool.call(format!(r#"{{"n":{n}}}"#)).await.unwrap();
        assert_eq!(output, format!(r#"{{"n":{n}}}"#));
    }
    assert_eq!(
        executor.admitted_len(),
        1,
        "a second call reuses the admission"
    );
}

#[test]
fn a_pin_that_no_longer_matches_the_installed_plugin_is_refused() {
    let (home, _) = installed_echo();
    let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
    let stale = format!("sha256:{}", "0".repeat(64));
    let error = PluginTool::resolve(executor, &tool_ref(Some(&stale)))
        .err()
        .expect("a stale pin must not resolve");
    assert!(format!("{error:#}").contains("not the pinned"), "{error:#}");
}

#[test]
fn a_plugin_that_is_not_installed_is_refused() {
    let (home, _) = installed_echo();
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    assert!(executor.resolve("team/missing", None).is_err());
    assert!(executor.resolve("../escape", None).is_err());
    assert!(PluginExecutor::default()
        .resolve("team/plugin", None)
        .is_err());
}

#[tokio::test]
async fn a_changed_grant_is_admitted_again() {
    let (home, mut record) = installed_echo();
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    executor.call(&record, serde_json::json!(1)).await.unwrap();
    record.granted = Some(crate::plugin::Manifold::sealed());
    let call = executor.call(&record, serde_json::json!(2)).await.unwrap();
    assert_eq!(call.outcome.output, serde_json::json!(2));
    assert_eq!(
        executor.admitted_len(),
        1,
        "the new grant replaces the old admission"
    );
}
