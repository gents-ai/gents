#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InferenceCallSlotRow<'a> {
    pub(crate) backend_id: &'a str,
    pub(crate) call_state: &'a str,
}

impl<'a> InferenceCallSlotRow<'a> {
    pub(crate) fn new(backend_id: &'a str, call_state: &'a str) -> Self {
        Self {
            backend_id,
            call_state,
        }
    }
}

fn call_state_holds_backend_slot(call_state: &str) -> bool {
    call_state == "running"
}

pub(crate) fn slot_contribution(row: InferenceCallSlotRow<'_>, backend_id: &str) -> usize {
    if row.backend_id == backend_id && call_state_holds_backend_slot(row.call_state) {
        1
    } else {
        0
    }
}

pub(crate) fn reconstructed_running_slot_count<'a>(
    rows: impl IntoIterator<Item = InferenceCallSlotRow<'a>>,
    backend_id: &str,
) -> usize {
    rows.into_iter()
        .map(|row| slot_contribution(row, backend_id))
        .sum()
}
