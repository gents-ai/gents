#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ToolCallState {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::TimedOut,
        Self::Cancelled,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "timedOut" => Some(Self::TimedOut),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CancelCause {
    Interrupted,
    Deadline,
    UserCancelled,
}

impl CancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelCause::Interrupted => "interrupted",
            CancelCause::Deadline => "deadline",
            CancelCause::UserCancelled => "userCancelled",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "interrupted" => Some(CancelCause::Interrupted),
            "deadline" => Some(CancelCause::Deadline),
            "userCancelled" => Some(CancelCause::UserCancelled),
            _ => None,
        }
    }

    pub const ALL: &'static [CancelCause] = &[
        CancelCause::Interrupted,
        CancelCause::Deadline,
        CancelCause::UserCancelled,
    ];
}
