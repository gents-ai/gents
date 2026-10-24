use serde::{Deserialize, Serialize};

use super::error::DocumentConfigError;

type Result<T, E = DocumentConfigError> = std::result::Result<T, E>;

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
        missed_run_policy: Option<crate::schedule_cron::CronMissedRunPolicy>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleObservation {
    pub trigger_id: String,
    pub next_run_at: Option<String>,
}

impl ScheduleCadence {
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        match self {
            Self::Interval { interval_secs } => {
                if *interval_secs < 1 {
                    return Err(DocumentConfigError::ScheduleIntervalNotPositive {
                        interval_secs: *interval_secs,
                    });
                }
                Ok(())
            }
            Self::Cron {
                expression,
                timezone,
                missed_run_policy,
            } => {
                if expression.trim().is_empty() {
                    return Err(DocumentConfigError::CronExpressionBlank);
                }
                if timezone.trim().is_empty() {
                    return Err(DocumentConfigError::CronTimezoneBlank);
                }
                crate::schedule_cron::validate_cron_schedule(
                    expression,
                    timezone,
                    missed_run_policy.map(|policy| match policy {
                        crate::schedule_cron::CronMissedRunPolicy::LatestOnly => "latest_only",
                    }),
                )?;
                Ok(())
            }
        }
    }
}
