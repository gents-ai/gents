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
        return if main && runtime_active {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_macos_only_the_main_view_hides_for_a_running_runtime() {
        assert_eq!(
            close_action(false, true, true, true, false),
            CloseAction::Hide
        );
        assert_eq!(
            close_action(false, true, false, true, false),
            CloseAction::Close
        );
        assert_eq!(
            close_action(false, false, true, false, false),
            CloseAction::Close
        );
    }

    #[test]
    fn on_macos_main_hides_while_other_views_need_it() {
        assert_eq!(
            close_action(true, true, false, true, false),
            CloseAction::Hide
        );
        assert_eq!(
            close_action(true, false, false, false, true),
            CloseAction::CloseWithHiddenMain
        );
        assert_eq!(
            close_action(true, false, true, false, true),
            CloseAction::Close
        );
    }
}
