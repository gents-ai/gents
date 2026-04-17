use super::*;

fn wait_for_bootstrap_replication_ready(driver: &mut AuditDriver, label: &str) -> Result<()> {
    wait_for_replication_state(
        driver,
        label,
        Duration::from_secs(20),
        "subscriptions armed",
    )
}

pub(super) fn wait_for_live_deployment_docs_in_store(
    desktop_core: &ClientCore,
    deployment_label: &str,
    agent_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    wait_for_value(
        &format!("observed live deployment docs for {deployment_label}"),
        Duration::from_secs(120),
        || {
            let snapshot = desktop_core.store().snapshot();
            let has_principal = snapshot
                .agent_principals
                .iter()
                .any(|row| row.agent_did == agent_did);
            let has_behavior = snapshot
                .behaviors
                .iter()
                .any(|row| row.behavior_id == docs.behavior_id);
            let has_backend = snapshot
                .inference_backends
                .iter()
                .any(|row| row.backend_id == docs.backend_id);
            let has_tools = snapshot
                .tool_selections
                .iter()
                .any(|row| row.selection_id == docs.tool_selection_id);
            let has_profile = snapshot
                .inference_profiles
                .iter()
                .any(|row| row.profile_id == docs.inference_profile_id);
            (has_principal && has_behavior && has_backend && has_tools && has_profile).then_some(())
        },
    )
}

pub(super) fn wait_for_bootstrap_chat_ready(
    driver: &mut AuditDriver,
    peer_id: &str,
    agent_did: &str,
) -> Result<()> {
    wait_for_bootstrap_replication_ready(driver, "desktop bootstrap status")?;
    let deployment_target = audit::targets::chat_deployment(peer_id);
    driver.wait_for_target(
        "bootstrapped chat deployment row",
        Duration::from_secs(20),
        &deployment_target,
    )?;
    driver.click_target(&deployment_target);
    wait_for_value(
        "bootstrapped chat selection",
        Duration::from_secs(10),
        || {
            (driver.app.state.chat.shell.selected_peer_id.as_deref() == Some(peer_id)
                && driver.app.state.chat.shell.selected_agent_did.as_deref() == Some(agent_did))
            .then_some(())
        },
    )
}

pub(super) fn wait_for_bootstrap_chat_rows(
    driver: &mut AuditDriver,
    deployments: &[LiveRemoteDeployment],
) -> Result<()> {
    wait_for_bootstrap_replication_ready(driver, "desktop multi-agent bootstrap status")?;
    for deployment in deployments {
        let deployment_target = audit::targets::chat_deployment(&deployment.peer_id);
        driver.wait_for_target(
            &format!("bootstrapped chat deployment row for {}", deployment.label),
            Duration::from_secs(20),
            &deployment_target,
        )?;
    }
    Ok(())
}
