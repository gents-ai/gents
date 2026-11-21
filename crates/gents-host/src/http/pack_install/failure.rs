use gents_protocol::operations::pack::hosted::{HostedInstallFailure, HostedInstallOutcome};

/// A failed hosted pack session: what crosses the wire and what the runtime
/// logs. The error chain is dropped where the failure is made, because it can
/// quote install values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionFailure {
    pub class: HostedInstallFailure,
    pub outcome: HostedInstallOutcome,
}

impl SessionFailure {
    pub fn not_installed<E>(class: HostedInstallFailure) -> impl FnOnce(E) -> Self
    where
        E: Into<anyhow::Error>,
    {
        move |error| Self::of(class, HostedInstallOutcome::NotInstalled, error.into())
    }

    pub fn unconfirmed<E>(class: HostedInstallFailure) -> impl FnOnce(E) -> Self
    where
        E: Into<anyhow::Error>,
    {
        move |error| Self::of(class, HostedInstallOutcome::Unconfirmed, error.into())
    }

    fn of(class: HostedInstallFailure, outcome: HostedInstallOutcome, error: anyhow::Error) -> Self {
        let class = if gents_store::config_client::user_write::UserWriteRefusal::in_chain(
            error.as_ref(),
        ) {
            HostedInstallFailure::RefusedForAPerson
        } else {
            class
        };
        Self { class, outcome }
    }

    pub fn log(&self) {
        tracing::warn!(class = %self.class, outcome = %self.outcome, "hosted pack session failed");
    }

    pub fn message(&self) -> serde_json::Value {
        serde_json::json!({"type": "failed", "class": self.class, "outcome": self.outcome})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_sends_and_logs_its_class_never_its_chain() {
        let secret = "sk-live-0123456789";
        for class in HostedInstallFailure::ALL {
            let failure =
                SessionFailure::not_installed(class)(anyhow::anyhow!("TOKEN resolved to {secret}"));
            assert_eq!(
                failure,
                SessionFailure {
                    class,
                    outcome: HostedInstallOutcome::NotInstalled
                }
            );
            let message = failure.message();
            assert!(!message.to_string().contains(secret), "{message}");
            assert_eq!(message["class"], serde_json::to_value(class).unwrap());
        }
    }

    #[test]
    fn a_refused_write_for_a_person_is_its_own_class_whatever_step_failed() {
        let refusal = anyhow::Error::new(gents_store::config_client::user_write::RefreshRefused)
            .context("applying the plan");
        let failure = SessionFailure::unconfirmed(HostedInstallFailure::ApplyUnconfirmed)(refusal);
        assert_eq!(failure.class, HostedInstallFailure::RefusedForAPerson);
        assert_eq!(failure.outcome, HostedInstallOutcome::Unconfirmed);
    }
}
