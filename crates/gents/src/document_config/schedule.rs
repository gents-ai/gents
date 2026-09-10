use serde::{Deserialize, Serialize};

/// Reusable schedule configuration. Task selection and concurrency live on
/// Trigger; each referencing trigger has its own scheduling cursor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Schedule {
    pub agent_did: String,
    pub schedule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    pub cadence: ScheduleCadence,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub updated_at: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// An interval or a timezone-qualified cron expression, never both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ScheduleCadence {
    Interval {
        /// Must be positive. Existing interval catch-up behavior is preserved.
        interval_secs: i64,
    },
    Cron {
        expression: String,
        timezone: String,
        /// Omitted means latest_only, as in the existing cron implementation.
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        missed_run_policy: Option<crate::schedule_cron::CronMissedRunPolicy>,
    },
}

/// Runtime scheduling cursor, keyed by trigger rather than reusable schedule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleObservation {
    pub trigger_id: String,
    pub next_run_at: Option<String>,
}

impl ScheduleCadence {
    /// Shared cadence admission and runtime validation through the cron owner.
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::Interval { interval_secs } => {
                anyhow::ensure!(
                    *interval_secs >= 1,
                    "schedule interval_secs must be >= 1; got {interval_secs}"
                );
                Ok(())
            }
            Self::Cron {
                expression,
                timezone,
                missed_run_policy,
            } => {
                anyhow::ensure!(
                    !expression.trim().is_empty(),
                    "cron schedule requires a non-empty expression"
                );
                anyhow::ensure!(
                    !timezone.trim().is_empty(),
                    "cron schedule requires timezone"
                );
                crate::schedule_cron::validate_cron_schedule(
                    expression,
                    timezone,
                    missed_run_policy.map(|policy| match policy {
                        crate::schedule_cron::CronMissedRunPolicy::LatestOnly => "latest_only",
                    }),
                )
            }
        }
    }
}

#[cfg(test)]
#[path = "schedule_cadence_tests.rs"]
mod cadence_tests;
