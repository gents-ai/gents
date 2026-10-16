#[derive(Debug, PartialEq, Eq)]
pub(super) enum CloseAction {
    Close,
    Hide,
    CloseWithHiddenMain,
}

pub(super) fn close_action(
    mac: bool,
    main: bool,
    runtime_active: bool,
    other_views: bool,
    main_hidden: bool,
) -> CloseAction {
    if !mac {
        return if runtime_active {
            CloseAction::Hide
        } else {
            CloseAction::Close
        };
    }
    if main && (runtime_active || other_views) {
        CloseAction::Hide
    } else if !main && !runtime_active && !other_views && main_hidden {
        CloseAction::CloseWithHiddenMain
    } else {
        CloseAction::Close
    }
}
