use rapidhash::RapidHashMap;

pub(in crate::runtime::agent) const UNRESOLVED_SESSION_ATTEMPT_BOUND: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime::agent) enum SessionSelection {
    Unavailable,
    Matched,
    Mismatched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runtime::agent) enum SessionSelectionDisposition {
    Claim,
    Wait,
    Reject,
}

pub(in crate::runtime::agent) fn session_selection_disposition(
    selection: SessionSelection,
    attempts: u32,
    bound: u32,
) -> SessionSelectionDisposition {
    match selection {
        SessionSelection::Matched => SessionSelectionDisposition::Claim,
        SessionSelection::Mismatched => SessionSelectionDisposition::Reject,
        SessionSelection::Unavailable => {
            if bound <= attempts {
                SessionSelectionDisposition::Reject
            } else {
                SessionSelectionDisposition::Wait
            }
        }
    }
}

#[derive(Default)]
pub(in crate::runtime::agent) struct UnresolvedSessions(RapidHashMap<String, u32>);

impl UnresolvedSessions {
    pub(in crate::runtime::agent) fn record(&mut self, request_id: &str) -> u32 {
        let attempts = self.0.entry(request_id.to_owned()).or_insert(0);
        *attempts += 1;
        *attempts
    }

    pub(in crate::runtime::agent) fn forget(&mut self, request_id: &str) {
        self.0.remove(request_id);
    }
}
