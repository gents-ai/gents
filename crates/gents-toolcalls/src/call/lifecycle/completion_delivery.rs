use gents_protocol::tool_call::ToolCallState;

const DELIVERED: &str = "completed";
const PENDING: &str = "completionPending";
const PENDING_REASON_PREFIX: &str = "completionPending:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionDelivery<'a> {
    Delivered,
    Pending { reason: Option<&'a str> },
}

impl<'a> CompletionDelivery<'a> {
    pub fn parse(status: &'a str) -> Option<Self> {
        match status {
            DELIVERED => Some(Self::Delivered),
            PENDING => Some(Self::Pending { reason: None }),
            _ => status
                .strip_prefix(PENDING_REASON_PREFIX)
                .map(|reason| Self::Pending {
                    reason: Some(reason),
                }),
        }
    }

    pub fn render(self) -> String {
        match self {
            Self::Delivered => DELIVERED.to_owned(),
            Self::Pending { reason: None } => PENDING.to_owned(),
            Self::Pending {
                reason: Some(reason),
            } => format!("{PENDING_REASON_PREFIX}{reason}"),
        }
    }
}

pub fn owes_completion_delivery(state: ToolCallState, status: Option<&str>) -> bool {
    state.is_terminal()
        && matches!(
            status.and_then(CompletionDelivery::parse),
            Some(CompletionDelivery::Pending { .. })
        )
}

#[cfg(test)]
mod tests {
    use super::CompletionDelivery;

    #[test]
    fn completion_delivery_status_round_trips_and_rejects_other_words() {
        for delivery in [
            CompletionDelivery::Delivered,
            CompletionDelivery::Pending { reason: None },
            CompletionDelivery::Pending { reason: Some("") },
            CompletionDelivery::Pending {
                reason: Some("assignment_active"),
            },
        ] {
            assert_eq!(
                CompletionDelivery::parse(&delivery.render()),
                Some(delivery)
            );
        }
        for status in ["", "completionPendingX", "Completed", "completionpending"] {
            assert_eq!(CompletionDelivery::parse(status), None, "{status}");
        }
    }
}
