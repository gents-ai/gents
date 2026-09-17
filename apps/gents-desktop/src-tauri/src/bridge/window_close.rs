//! Preserve the single-window lifecycle while allowing macOS sibling views.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_main_preserves_exit_and_background_runtime_behavior() {
        for mac in [false, true] {
            assert_eq!(
                close_action(mac, true, false, false, false),
                CloseAction::Close
            );
            assert_eq!(
                close_action(mac, true, true, false, false),
                CloseAction::Hide
            );
        }
    }

    #[test]
    fn mac_siblings_retain_coordinator_only_while_needed() {
        assert_eq!(
            close_action(true, true, false, true, false),
            CloseAction::Hide
        );
        assert_eq!(
            close_action(true, false, false, true, true),
            CloseAction::Close
        );
        assert_eq!(
            close_action(true, false, true, false, true),
            CloseAction::Close
        );
        assert_eq!(
            close_action(true, false, false, false, true),
            CloseAction::CloseWithHiddenMain
        );
        assert_eq!(
            close_action(true, false, false, false, false),
            CloseAction::Close
        );
    }

    #[test]
    fn linux_and_windows_keep_existing_close_policy() {
        for main in [false, true] {
            for siblings in [false, true] {
                assert_eq!(
                    close_action(false, main, true, siblings, false),
                    CloseAction::Hide
                );
                assert_eq!(
                    close_action(false, main, false, siblings, true),
                    CloseAction::Close
                );
            }
        }
    }
}
