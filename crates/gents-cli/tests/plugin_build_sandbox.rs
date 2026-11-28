//! The real compiler path of `config pack build`: `gents __plugin-build`
//! started under the command sandbox, for a scaffolded Rust plugin.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use gents_pack::pack::authoring::{pack_built, scaffold, ScaffoldInput, Template};

#[tokio::test]
async fn a_scaffolded_plugin_builds_and_passes_its_cases_inside_the_sandbox() {
    if !gents_session::workspace::sandbox::workspace_write_sandbox_enforced() {
        return;
    }
    // SAFETY: this is the only test in this binary, so nothing reads the
    // environment concurrently.
    unsafe { std::env::set_var("GENTS_PLUGIN_BUILDER", env!("CARGO_BIN_EXE_gents")) };
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let dir = root.join("echo_tool");
    scaffold(
        &dir,
        "echo_tool",
        &ScaffoldInput {
            namespace: "acme".into(),
            template: Some(Template::PluginTool),
            kind: None,
            language: Some("rust".into()),
            ux: false,
        },
    )
    .unwrap();

    let build = gents_tools::native::run_plugin_build(
        &gents_loop::tool::ToolCallEnvelope::default(),
        &root,
        &dir,
        true,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("{error:#}"));
    let expected_sandbox = if cfg!(target_os = "macos") {
        "macos_seatbelt"
    } else {
        "linux_landlock"
    };
    assert_eq!(build.sandbox, expected_sandbox);
    assert_eq!(build.output["cases"][0]["passed"], 1, "{}", build.output);
    assert_eq!(
        build.output["cases"][0]["failures"],
        serde_json::json!([]),
        "{}",
        build.output
    );
    assert!(dir.join("plugins/echo_tool.afb").is_file());
    let report = pack_built(&dir, None).unwrap();
    assert!(report.out.is_file());
}
