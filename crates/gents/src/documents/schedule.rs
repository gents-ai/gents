use serde::{Deserialize, Serialize};

/// Each referencing trigger keeps its own scheduling cursor against this
/// shared cadence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct Schedule {
    pub node_did: String,
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
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ScheduleCadence {
    Interval {
        interval_secs: i64,
    },
    Cron {
        expression: String,
        timezone: String,
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        missed_run_policy: Option<crate::documents::schedule_cron::CronMissedRunPolicy>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleObservation {
    pub trigger_id: String,
    pub next_run_at: Option<String>,
}

impl ScheduleCadence {
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
                crate::documents::schedule_cron::validate_cron_schedule(
                    expression,
                    timezone,
                    missed_run_policy.map(|policy| match policy {
                        crate::documents::schedule_cron::CronMissedRunPolicy::LatestOnly => {
                            "latest_only"
                        }
                    }),
                )
            }
        }
    }
}
