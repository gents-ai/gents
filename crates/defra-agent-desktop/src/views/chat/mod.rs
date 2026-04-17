mod composer;
mod container;
mod header;
mod nudge;
mod sidebar;
mod transcript;
mod view_model;

pub use container::{prepare_state, send_disabled, show_main, show_sidebar, turn_state_label};
pub use transcript::markdown_theme_names;

pub use view_model::{
    build_conversation_buckets, build_deployment_entries, ConversationBucket, ConversationEntry,
    DeploymentEntry,
};

#[cfg(test)]
mod tests {
    use tokio::runtime::Runtime;

    use crate::chat::controller;
    use crate::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    use crate::state::ShellState;

    #[test]
    fn create_first_conversation_selects_new_session() -> anyhow::Result<()> {
        let runtime = Runtime::new()?;
        let tempdir = tempfile::tempdir()?;
        let core = runtime.block_on(ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(tempdir.path()),
            ClientCoreOptions::local_only(),
        ))?;

        let principal_resp = runtime.block_on(core.node().execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        ));
        assert!(!principal_resp.has_errors());

        let mut state = ShellState::default();
        state.chat.shell.selected_agent_did = Some("did:defra:amy".to_string());
        controller::create_conversation(&mut state.chat, Some(&core), &runtime)?;

        assert!(state.chat.shell.selected_session_id.is_some());
        assert_eq!(core.store().snapshot().conversations.len(), 1);
        runtime.block_on(core.shutdown())?;
        Ok(())
    }
}
