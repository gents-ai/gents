use anyhow::Result;
use gents::document_config::InferenceProfile;
use serde_json::json;

use crate::cli::*;
use crate::config_writes::{write_inference_profile_document, ConfigAccess};
use crate::print_json;

/// Decode this command's args into the document type
/// `InferenceProfile::validate` owns. The two are field-for-field identical.
fn to_document_profile(args: &InferenceProfileUpsertArgs) -> InferenceProfile {
    InferenceProfile {
        profile_id: args.profile_id.clone(),
        display_name: args.display_name.clone(),
        context_window: args.context_window,
        max_output_tokens: args.max_output_tokens,
        max_turns: args.max_turns,
        temperature: args.temperature,
        top_p: args.top_p,
        top_k: args.top_k,
        seed: args.seed,
        min_p: args.min_p,
        frequency_penalty: args.frequency_penalty,
        presence_penalty: args.presence_penalty,
        repetition_penalty: args.repetition_penalty,
        reasoning_effort: args.reasoning_effort.clone(),
        stream_batch_ms: args.stream_batch_ms,
        stream_liveness_timeout_secs: args.stream_liveness_timeout_secs,
        deadline_duration_secs: args.deadline_duration_secs,
        retry_max_transport: args.retry_max_transport,
        retry_backoff_ms: args.retry_backoff_ms.clone(),
        retry_max_resample: args.retry_max_resample,
        retry_allow_repair: args.retry_allow_repair,
        retry_interactive_max: args.retry_interactive_max,
    }
}

pub(super) async fn inference_profile_set(args: InferenceProfileUpsertArgs) -> Result<()> {
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let doc_id = write_inference_profile_document(&access, &to_document_profile(&args)).await?;
    let output = json!({
        "doc_id": doc_id,
        "profile_id": args.profile_id,
        "display_name": args.display_name,
        "context_window": args.context_window,
        "max_output_tokens": args.max_output_tokens,
        "max_turns": args.max_turns,
        "temperature": args.temperature,
        "top_p": args.top_p,
        "top_k": args.top_k,
        "seed": args.seed,
        "min_p": args.min_p,
        "frequency_penalty": args.frequency_penalty,
        "presence_penalty": args.presence_penalty,
        "repetition_penalty": args.repetition_penalty,
        "reasoning_effort": args.reasoning_effort,
        "stream_batch_ms": args.stream_batch_ms,
        "stream_liveness_timeout_secs": args.stream_liveness_timeout_secs,
        "deadline_duration_secs": args.deadline_duration_secs,
        "retry_max_transport": args.retry_max_transport,
        "retry_backoff_ms": args.retry_backoff_ms,
        "retry_max_resample": args.retry_max_resample,
        "retry_allow_repair": args.retry_allow_repair,
        "retry_interactive_max": args.retry_interactive_max,
    });
    print_json(&output)?;
    Ok(())
}
