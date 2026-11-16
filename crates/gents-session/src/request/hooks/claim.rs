/// Lean `Hooks.Successor`: the request that retires a pending one, named by
/// the plugin that asked for the retirement so core can record it on the
/// superseded row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Successor {
    pub request_id: String,
    pub request_doc_id: String,
}

/// Lean `Hooks.ClaimVerdict`. A plugin names the outcome; core performs the
/// transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimVerdict {
    Allow,
    LeaveQueued,
    Retire {
        reason: String,
        successor: Successor,
    },
    Reject {
        reason: String,
    },
}

impl ClaimVerdict {
    /// Lean `Hooks.ClaimVerdict.severity`.
    pub fn severity(&self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::LeaveQueued => 1,
            Self::Retire { .. } => 2,
            Self::Reject { .. } => 3,
        }
    }
}

/// Lean `Hooks.combine`: the most severe verdict wins; the first plugin in
/// the fixed order supplies the reason among equals.
pub fn combine<I>(verdicts: I) -> ClaimVerdict
where
    I: IntoIterator<Item = ClaimVerdict>,
{
    verdicts
        .into_iter()
        .fold(ClaimVerdict::Allow, |current, next| {
            if current.severity() < next.severity() {
                next
            } else {
                current
            }
        })
}
