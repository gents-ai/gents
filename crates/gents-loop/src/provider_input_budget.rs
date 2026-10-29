#[derive(Debug, thiserror::Error)]
pub enum ContextBudgetError {
    #[error(
        "provider_input_has_no_output_capacity: estimated_input_tokens={estimated_input_tokens}, \
         context_window={context_window}, effective_max_output_tokens={effective_max_output_tokens}"
    )]
    NoOutputCapacity {
        estimated_input_tokens: usize,
        context_window: usize,
        effective_max_output_tokens: usize,
    },
}

pub fn threshold_basis_points(threshold: f64) -> u64 {
    if !threshold.is_finite() || threshold <= 0.0 {
        return 0;
    }
    (threshold * 10_000.0).round().min(10_000.0) as u64
}

pub fn threshold_budget(context_window: usize, threshold: f64) -> usize {
    let basis_points = u128::from(threshold_basis_points(threshold));
    ((context_window as u128 * basis_points) / 10_000) as usize
}

pub fn effective_input_budget(context_window: usize, threshold: f64) -> usize {
    threshold_budget(context_window, threshold).min(context_window)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThresholdDecision {
    NotNeeded,
    ReduceEligible,
}

pub fn threshold_decision(input_tokens: usize, effective_input_budget: usize) -> ThresholdDecision {
    if input_tokens <= effective_input_budget {
        ThresholdDecision::NotNeeded
    } else {
        ThresholdDecision::ReduceEligible
    }
}

pub fn configured_output_ceiling(max_tokens: Option<u64>) -> usize {
    max_tokens
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX)
}

/// The recovery band: the whole request right after a reduction (fixed
/// layers, rolling checkpoint and retained tail) fits this share of the
/// compaction budget, so the next turns grow back toward the threshold
/// instead of crossing it again straight away.
pub const COMPACTION_RECOVERY_BAND_BASIS_POINTS: u64 = 8_000;

/// The recovery band of `effective_input_budget`, rounded down: four fifths.
pub fn recovery_band_budget(effective_input_budget: usize) -> usize {
    ((effective_input_budget as u128 * u128::from(COMPACTION_RECOVERY_BAND_BASIS_POINTS))
        / u128::from(UNIT_INPUT_RATIO_BASIS_POINTS)) as usize
}

/// The retained tail's budget after a reduction: the configured keep-recent
/// cap, else what the recovery band leaves after the fixed layers and the
/// checkpoint's reserve (the summary call's output cap), floored at zero. So
/// `fixed + summary_reserve + tail <= 4/5 * budget` whenever the fixed layers
/// and reserve fit the band at all. Modelled by
/// `PromptAssembly.Budget.compactionRetentionTarget`.
pub fn compaction_retention_target(
    configured_keep_recent: usize,
    effective_input_budget: usize,
    fixed_input_tokens: usize,
    summary_reserve_tokens: usize,
) -> usize {
    configured_keep_recent.min(
        recovery_band_budget(effective_input_budget)
            .saturating_sub(fixed_input_tokens)
            .saturating_sub(summary_reserve_tokens),
    )
}

pub fn summary_output_ceiling(configured_max_output_tokens: usize, context_window: usize) -> usize {
    configured_max_output_tokens.min(context_window.div_ceil(4).max(1))
}

/// Input budget for one rolling summary request, in provider tokens: the
/// window less the summary's output. Callers compare it with calibrated
/// estimates, never the raw estimator count.
pub fn rolling_summary_input_budget(
    context_window: usize,
    configured_max_output_tokens: usize,
) -> usize {
    if configured_max_output_tokens < context_window {
        context_window - configured_max_output_tokens
    } else {
        context_window
    }
}

pub fn effective_output_budget(
    input_tokens: usize,
    context_window: usize,
    configured_max_output_tokens: usize,
) -> usize {
    configured_max_output_tokens.min(context_window.saturating_sub(input_tokens))
}

pub fn can_dispatch(
    input_tokens: usize,
    context_window: usize,
    configured_max_output_tokens: usize,
) -> bool {
    let output_tokens =
        effective_output_budget(input_tokens, context_window, configured_max_output_tokens);
    output_tokens > 0
        && input_tokens
            .checked_add(output_tokens)
            .is_some_and(|total| total <= context_window)
}

/// One provider token per estimated token, in basis points.
pub const UNIT_INPUT_RATIO_BASIS_POINTS: u64 = 10_000;

/// The largest provider/estimate ratio an observation may set. A single
/// malformed usage report must not make every later request look several
/// times its size.
pub const MAX_INPUT_RATIO_BASIS_POINTS: u64 = 40_000;

/// Provider tokens per estimated token, in basis points, when the provider
/// counted `observed_input_tokens` for a request this estimator counted as
/// `estimated_input_tokens`. Floored at one: the ratio only ever raises an
/// estimate.
pub fn observed_input_ratio_basis_points(
    observed_input_tokens: u64,
    estimated_input_tokens: usize,
) -> Option<u64> {
    if observed_input_tokens == 0 || estimated_input_tokens == 0 {
        return None;
    }
    let ratio = (u128::from(observed_input_tokens) * u128::from(UNIT_INPUT_RATIO_BASIS_POINTS))
        .div_ceil(estimated_input_tokens as u128);
    Some(ratio.clamp(
        u128::from(UNIT_INPUT_RATIO_BASIS_POINTS),
        u128::from(MAX_INPUT_RATIO_BASIS_POINTS),
    ) as u64)
}

/// `budget` provider tokens in this estimator's units: the largest estimate
/// whose scaled count stays within `budget`.
pub fn estimator_input_budget(budget: usize, ratio_basis_points: u64) -> usize {
    let ratio = ratio_basis_points.max(UNIT_INPUT_RATIO_BASIS_POINTS);
    ((budget as u128 * u128::from(UNIT_INPUT_RATIO_BASIS_POINTS)) / u128::from(ratio)) as usize
}

/// `tokens` provider tokens in this estimator's units at `ratio_basis_points`,
/// rounded up.
pub fn estimator_input_units(tokens: usize, ratio_basis_points: u64) -> usize {
    let ratio = ratio_basis_points.max(UNIT_INPUT_RATIO_BASIS_POINTS);
    usize::try_from(
        (tokens as u128 * u128::from(UNIT_INPUT_RATIO_BASIS_POINTS)).div_ceil(u128::from(ratio)),
    )
    .unwrap_or(usize::MAX)
}

/// The input budget compaction keeps a request within, in provider tokens:
/// the configured threshold of `context_window`, and never more than the
/// window less the output the request reserves. The reserve is the
/// configured max output; an unbounded or implausible one (over a quarter of
/// the window) reserves a quarter. After a provider overflow,
/// `context_window` is the provider's stated limit. Modelled by
/// `PromptAssembly.Budget.compactionInputBudget`.
pub fn compaction_input_budget(
    context_window: usize,
    threshold: f64,
    configured_max_output_tokens: usize,
) -> usize {
    let reserve = configured_max_output_tokens.min(context_window / 4);
    effective_input_budget(context_window, threshold).min(context_window.saturating_sub(reserve))
}

/// Provider tokens per estimated token, in basis points, assumed for Claude
/// Messages before any provider usage is observed. Serialized wire bytes / 4
/// undercounts Claude's tokenizer: live Claude Opus calls reported 1.57x to
/// 1.79x the estimate (65,755 estimated, 109,881 counted; ~561k estimated,
/// 1,001,685 counted), so a 0.75 threshold could not fire before a 1M limit.
pub const CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS: u64 = 18_000;

/// `estimated_input_tokens` scaled by `ratio_basis_points`, rounded up.
pub fn calibrated_input_tokens(estimated_input_tokens: usize, ratio_basis_points: u64) -> usize {
    let ratio = ratio_basis_points.max(UNIT_INPUT_RATIO_BASIS_POINTS);
    usize::try_from(
        (estimated_input_tokens as u128 * u128::from(ratio))
            .div_ceil(u128::from(UNIT_INPUT_RATIO_BASIS_POINTS)),
    )
    .unwrap_or(usize::MAX)
}

/// The provider's own count of one completed call: the input it counted
/// (cache reads and writes included), the output it produced, and this
/// estimator's raw count of the request it answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextAnchor {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_input_tokens: usize,
}

/// A request's context size in provider tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextEstimate {
    /// This estimator's raw count of the request.
    pub raw: usize,
    /// The provider/estimate ratio applied, in basis points.
    pub ratio_basis_points: u64,
    /// `raw` scaled by the calibration ratio.
    pub calibrated: usize,
    /// The anchor's provider count plus the calibrated growth since it, when
    /// a current anchor applies.
    pub anchored: Option<usize>,
    /// What every decision uses: the larger of `anchored` and `calibrated`,
    /// so a missing or stale anchor never lowers the count.
    pub tokens: usize,
}

#[derive(Clone, Debug)]
struct CalibrationState {
    /// The ratio before any observation.
    prior_basis_points: u64,
    /// The latest usable observations, oldest first, at most
    /// [`CALIBRATION_OBSERVATION_WINDOW`].
    observations: std::collections::VecDeque<u64>,
    anchor: Option<ContextAnchor>,
}

impl CalibrationState {
    /// The largest recent observation, else the prior: a higher observation
    /// takes effect at once, a lower one only once the higher ones age out of
    /// the window. Overestimating costs an earlier compaction;
    /// underestimating can overflow the provider and end the request.
    fn ratio_basis_points(&self) -> u64 {
        self.observations
            .iter()
            .copied()
            .max()
            .unwrap_or(self.prior_basis_points)
    }

    fn push(&mut self, ratio_basis_points: u64) {
        if self.observations.len() == CALIBRATION_OBSERVATION_WINDOW {
            self.observations.pop_front();
        }
        self.observations.push_back(ratio_basis_points);
    }
}

/// Observations the ratio is the maximum of, per session and model.
pub const CALIBRATION_OBSERVATION_WINDOW: usize = 8;

fn clamp_ratio(ratio_basis_points: u64) -> u64 {
    ratio_basis_points.clamp(UNIT_INPUT_RATIO_BASIS_POINTS, MAX_INPUT_RATIO_BASIS_POINTS)
}

/// How this estimator's count maps to the provider's count for one session
/// and model.
///
/// The anchor is the session's latest completed call: the provider counted
/// `input + output` for everything up to and including that response, so a
/// later request costs that much plus its growth past the anchored request,
/// net of the replayed response. Only the growth is estimated, scaled by the
/// ratio: the largest provider/estimate ratio among the last
/// [`CALIBRATION_OBSERVATION_WINDOW`] observations, floored at one, or the
/// profile prior before any observation. A request smaller than the anchored
/// one has been reduced since and is not anchored. Clones share the state, so
/// the loop's observations reach the owner that seeded it.
#[derive(Clone, Debug)]
pub struct ContextCalibration {
    state: std::sync::Arc<std::sync::Mutex<CalibrationState>>,
    anchoring: bool,
}

impl Default for ContextCalibration {
    fn default() -> Self {
        Self::with_ratio(UNIT_INPUT_RATIO_BASIS_POINTS)
    }
}

impl ContextCalibration {
    /// `ratio_basis_points` as the prior, with no observations.
    pub fn with_ratio(ratio_basis_points: u64) -> Self {
        Self::from_state(CalibrationState {
            prior_basis_points: clamp_ratio(ratio_basis_points),
            observations: std::collections::VecDeque::new(),
            anchor: None,
        })
    }

    /// `prior_basis_points` with `observations`, oldest first, already in
    /// the window: the session's recorded calls.
    pub fn with_observations(
        prior_basis_points: u64,
        observations: impl IntoIterator<Item = u64>,
    ) -> Self {
        let calibration = Self::with_ratio(prior_basis_points);
        {
            let mut state = calibration.lock();
            for ratio in observations {
                state.push(clamp_ratio(ratio));
            }
        }
        calibration
    }

    fn from_state(state: CalibrationState) -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(state)),
            anchoring: true,
        }
    }

    /// The same ratio state with `anchor` as the latest completed call.
    pub fn anchored_at(self, anchor: ContextAnchor) -> Self {
        self.lock().anchor = Some(anchor);
        self
    }

    /// The ratio to assume before this session has observed the provider.
    pub fn prior_for(profile: super::ProviderInputProfile) -> Self {
        Self::with_ratio(match profile {
            super::ProviderInputProfile::ClaudeMessages => {
                CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS
            }
            _ => UNIT_INPUT_RATIO_BASIS_POINTS,
        })
    }

    /// An independent copy of the ratio state that never anchors: for
    /// requests outside this session's transcript, such as summary chunks,
    /// whose size bears no relation to the anchored call.
    pub fn ratio_only(&self) -> Self {
        let mut state = self.lock().clone();
        state.anchor = None;
        Self {
            anchoring: false,
            ..Self::from_state(state)
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CalibrationState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn ratio_basis_points(&self) -> u64 {
        self.lock().ratio_basis_points()
    }

    pub fn anchor(&self) -> Option<ContextAnchor> {
        self.lock().anchor
    }

    /// Record that the provider counted `observed_input_tokens` for a request
    /// this estimator counted as `estimated_input_tokens`; returns the ratio
    /// now in effect when the observation was usable.
    pub fn observe(
        &self,
        observed_input_tokens: u64,
        estimated_input_tokens: usize,
    ) -> Option<u64> {
        let ratio =
            observed_input_ratio_basis_points(observed_input_tokens, estimated_input_tokens)?;
        let mut state = self.lock();
        state.push(ratio);
        Some(state.ratio_basis_points())
    }

    /// A completed call whose response was accepted into the transcript, so
    /// it is replayed in the next request: it becomes the anchor when this
    /// calibration anchors. A retracted attempt's response is not replayed
    /// and must not anchor.
    pub fn anchor_accepted(&self, anchor: ContextAnchor) {
        if self.anchoring {
            self.lock().anchor = Some(anchor);
        }
    }

    /// The transcript was reduced or rewritten: the anchored call no longer
    /// describes a prefix of the next request.
    pub fn invalidate_anchor(&self) {
        self.lock().anchor = None;
    }

    pub fn calibrated(&self, estimated_input_tokens: usize) -> usize {
        calibrated_input_tokens(estimated_input_tokens, self.ratio_basis_points())
    }

    /// The context size of a request this estimator counts as
    /// `estimated_input_tokens`. Modelled by
    /// `PromptAssembly.ContextCalibration.State.tokens`.
    pub fn context(&self, estimated_input_tokens: usize) -> ContextEstimate {
        let (ratio_basis_points, anchor) = {
            let state = self.lock();
            (state.ratio_basis_points(), state.anchor)
        };
        let calibrated = calibrated_input_tokens(estimated_input_tokens, ratio_basis_points);
        let anchored = anchor.filter(|_| self.anchoring).and_then(|anchor| {
            let growth = estimated_input_tokens.checked_sub(anchor.estimated_input_tokens)?;
            // The growth includes the anchored response as replayed; the
            // provider already counted it as output.
            let output = usize::try_from(anchor.output_tokens).unwrap_or(usize::MAX);
            let appended =
                calibrated_input_tokens(growth, ratio_basis_points).saturating_sub(output);
            Some(
                usize::try_from(anchor.input_tokens)
                    .unwrap_or(usize::MAX)
                    .saturating_add(output)
                    .saturating_add(appended),
            )
        });
        ContextEstimate {
            raw: estimated_input_tokens,
            ratio_basis_points,
            calibrated,
            anchored,
            tokens: anchored.map_or(calibrated, |anchored| anchored.max(calibrated)),
        }
    }

    /// `budget` provider tokens in this estimator's units.
    pub fn estimator_budget(&self, budget: usize) -> usize {
        estimator_input_budget(budget, self.ratio_basis_points())
    }

    /// `tokens` provider tokens in this estimator's units, rounded up.
    pub fn estimator_units(&self, tokens: usize) -> usize {
        estimator_input_units(tokens, self.ratio_basis_points())
    }
}

#[cfg(test)]
#[path = "provider_input_budget_tests.rs"]
mod tests;
