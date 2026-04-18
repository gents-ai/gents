use super::*;

pub(crate) fn assert_live_manage_switching_baseline(
    driver: &mut AuditDriver,
    alpha: &LiveDeploymentCase<'_>,
    alpha_submission: &LiveSubmissionCase,
    bravo: &LiveDeploymentCase<'_>,
    bravo_submission: &LiveSubmissionCase,
    backend: &AgentBackendConfig,
) -> Result<()> {
    open_chat_conversation_and_assert_isolation(
        driver,
        alpha,
        alpha_submission,
        bravo_submission.prompt.as_str(),
        "alpha transcript leaked bravo prompt after switching deployments",
    )?;
    open_chat_conversation_and_assert_isolation(
        driver,
        bravo,
        bravo_submission,
        alpha_submission.prompt.as_str(),
        "bravo transcript leaked alpha prompt after switching deployments",
    )?;

    assert_behavior_section(
        driver,
        alpha,
        bravo.docs.behavior_id.as_str(),
        "alpha behavior row after manage server switch",
    );
    assert_backend_section(
        driver,
        alpha,
        backend,
        "alpha backend row after manage server switch",
    )?;
    assert_profile_section(
        driver,
        alpha,
        "alpha inference profile row after manage server switch",
    )?;
    assert_request_timeline_section(
        driver,
        alpha,
        alpha_submission,
        bravo_submission.request_id.as_str(),
        "alpha request row after manage server switch",
    )?;

    assert_behavior_section(
        driver,
        bravo,
        alpha.docs.behavior_id.as_str(),
        "bravo behavior row after manage server switch",
    );
    assert_backend_section(
        driver,
        bravo,
        backend,
        "bravo backend row after manage server switch",
    )?;
    assert_profile_section(
        driver,
        bravo,
        "bravo inference profile row after manage server switch",
    )?;
    assert_request_timeline_section(
        driver,
        bravo,
        bravo_submission,
        alpha_submission.request_id.as_str(),
        "bravo request row after manage server switch",
    )
}

fn assert_behavior_section(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    hidden_behavior_id: &str,
    wait_label: &str,
) {
    let _ = open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::Behaviors,
        &deployment.docs.behavior_id,
        &[hidden_behavior_id],
        wait_label,
    )
    .expect("manage behavior row should be visible");
    assert_behavior_draft_bindings(
        driver,
        &deployment.docs.behavior_id,
        &deployment.agent_did,
        &deployment.docs.backend_id,
        &deployment.docs.tool_selection_id,
        &deployment.docs.inference_profile_id,
    );
}

fn assert_backend_section(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    backend: &AgentBackendConfig,
    wait_label: &str,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::Backends,
        &deployment.docs.backend_id,
        &[],
        wait_label,
    )?;
    assert_backend_draft_matches(
        driver,
        &deployment.docs.backend_id,
        backend.provider_kind.as_str(),
        backend.endpoint.as_str(),
        backend.model_name.as_str(),
    );
    Ok(())
}

fn assert_profile_section(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    wait_label: &str,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::InferenceProfiles,
        &deployment.docs.inference_profile_id,
        &[],
        wait_label,
    )?;
    assert_inference_profile_draft_matches(
        driver,
        &deployment.docs.inference_profile_id,
        "1024",
        "12",
    );
    Ok(())
}

fn assert_request_timeline_section(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
    submission: &LiveSubmissionCase,
    hidden_request_id: &str,
    wait_label: &str,
) -> Result<()> {
    let timeline_texts = open_manage_request_timeline_and_assert_visibility(
        driver,
        deployment,
        &submission.request_id,
        &[hidden_request_id],
        wait_label,
    )?;
    assert!(timeline_texts
        .iter()
        .any(|text| text.contains(submission.prompt.as_str())));
    assert!(timeline_texts
        .iter()
        .any(|text| text.contains(submission.response.trim())));
    Ok(())
}
