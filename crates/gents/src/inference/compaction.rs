#[cfg(test)]
#[path = "compaction/tests.rs"]
mod tests;

use std::sync::Arc;

use futures::future::BoxFuture;

pub use gents_loop::compaction::*;

pub(crate) fn reduction_options_for_agent(
    behavior: &crate::inference::resolved::ResolvedAgent,
) -> anyhow::Result<ReductionOptions> {
    let mut options = ReductionOptions {
        mode: behavior.compaction_strategy().reduction_mode(),
        ..ReductionOptions::default()
    };
    if let Some(config) = &behavior.compaction {
        fn setting(value: Option<i64>, fallback: usize, name: &str) -> anyhow::Result<usize> {
            match value {
                None => Ok(fallback),
                Some(value) if value > 0 => Ok(usize::try_from(value)?),
                Some(_) => anyhow::bail!("compaction {name} must be positive"),
            }
        }
        options.tool_result_max_chars = setting(
            config.tool_result_max_chars,
            options.tool_result_max_chars,
            "tool_result_max_chars",
        )?;
        options.keep_recent_tokens = setting(
            config.keep_recent_tokens,
            options.keep_recent_tokens,
            "keep_recent_tokens",
        )?;
        options.summary_max_output_tokens = setting(
            config.summary_max_output_tokens,
            options.summary_max_output_tokens,
            "summary_max_output_tokens",
        )?;
        options.summary_file_list_max = setting(
            config.summary_file_list_max,
            options.summary_file_list_max,
            "summary_file_list_max",
        )?;
    }
    Ok(options)
}

struct BackendScopedReductionEngine {
    inner: Arc<dyn ReductionEngine>,
    backend_id: String,
}

impl ReductionEngine for BackendScopedReductionEngine {
    fn retention_target(
        &self,
        configured_keep_recent: usize,
        messages: &[crate::inference::llm::message::Message],
        admission: ReductionAdmission,
        options: &ReductionOptions,
        context_window: usize,
    ) -> Result<usize, CompactionError> {
        self.inner.retention_target(
            configured_keep_recent,
            messages,
            admission,
            options,
            context_window,
        )
    }

    fn reduce<'a>(
        &'a self,
        messages: Vec<crate::inference::llm::message::Message>,
        context_window: usize,
        options: &'a ReductionOptions,
        admission: ReductionAdmission,
    ) -> BoxFuture<'a, Result<ReductionResult, CompactionError>> {
        let work = self
            .inner
            .reduce(messages, context_window, options, admission);
        let backend_id = self.backend_id.clone();
        Box::pin(async move { crate::inference::admission::scope_backend(&backend_id, work).await })
    }
}

pub(crate) fn backend_scoped_reduction_engine(
    inner: Arc<dyn ReductionEngine>,
    backend_id: String,
) -> Arc<dyn ReductionEngine> {
    Arc::new(BackendScopedReductionEngine { inner, backend_id })
}

#[cfg(test)]
use crate::provider_input::budget::{
    rolling_summary_input_budget, summary_output_ceiling, threshold_decision, ThresholdDecision,
};
