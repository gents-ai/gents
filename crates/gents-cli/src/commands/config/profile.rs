use crate::cli::InferenceProfileSetArgs;
use anyhow::{Context, Result};
use gents::document_config::InferenceProfile;
use serde_json::json;

fn decode_profile(contents: &[u8]) -> Result<InferenceProfile> {
    let profile: InferenceProfile =
        serde_json::from_slice(contents).context("decoding canonical InferenceProfile document")?;
    profile.validate()?;
    Ok(profile)
}

pub(super) async fn inference_profile_set(args: InferenceProfileSetArgs) -> Result<()> {
    let profile = decode_profile(
        &std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?,
    )?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let doc_id = gents::config_client::write_inference_profile_document(&access, &profile).await?;
    crate::print_json(
        &json!({"doc_id":doc_id,"agent_did":profile.agent_did,"profile_id":profile.profile_id,"backend_id":profile.backend_id,"model_name":profile.model_name}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_model_effort_and_policy_links_are_preserved() {
        let profile=decode_profile(br#"{"agent_did":"owner","profile_id":"chosen","backend_id":"provider","model_name":"exact-model","reasoning_effort":"high","sampling_id":"sampling","execution_id":"execution"}"#).unwrap();
        assert_eq!(profile.model_name, "exact-model");
        assert_eq!(profile.reasoning_effort, Some(gents::ReasoningEffort::High));
        assert_eq!(profile.sampling_id.as_deref(), Some("sampling"));
        assert_eq!(profile.execution_id.as_deref(), Some("execution"));
    }
    #[test]
    fn retired_flat_sampling_and_missing_owner_are_rejected() {
        for input in [
            r#"{"profile_id":"p","backend_id":"b","model_name":"m"}"#,
            r#"{"agent_did":"owner","profile_id":"p","backend_id":"b","model_name":"m","temperature":1}"#,
        ] {
            assert!(decode_profile(input.as_bytes()).is_err());
        }
    }
}
