#[derive(Debug, PartialEq, Eq)]
pub(super) enum CloseAction {
    Close,
    Hide,
    CloseWithHiddenMain,
}

/// `page`: a plugin's own window. Closing one closes that window and nothing
/// else: it never takes the main view, hidden or not, and so never the app.
pub(super) fn close_action(
    mac: bool,
    main: bool,
    page: bool,
    runtime_active: bool,
    other_views: bool,
    main_hidden: bool,
) -> CloseAction {
    if page {
        return CloseAction::Close;
    }
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
            close_action(false, true, false, true, true, false),
            CloseAction::Hide
        );
        assert_eq!(
            close_action(false, true, false, false, true, false),
            CloseAction::Close
        );
        assert_eq!(
            close_action(false, false, false, true, false, false),
            CloseAction::Close
        );
    }

    #[test]
    fn a_plugin_window_only_ever_closes_itself() {
        for mac in [true, false] {
            for runtime_active in [true, false] {
                for other_views in [true, false] {
                    for main_hidden in [true, false] {
                        assert_eq!(
                            close_action(
                                mac,
                                false,
                                true,
                                runtime_active,
                                other_views,
                                main_hidden
                            ),
                            CloseAction::Close
                        );
                    }
                }
            }
        }
        /* a view of the app, closed last beside a hidden main view, takes it */
        assert_eq!(
            close_action(true, false, false, false, false, true),
            CloseAction::CloseWithHiddenMain
        );
    }

    #[test]
    fn on_macos_main_hides_while_other_views_need_it() {
        assert_eq!(
            close_action(true, true, false, false, true, false),
            CloseAction::Hide
        );
        assert_eq!(
            close_action(true, false, false, false, false, true),
            CloseAction::CloseWithHiddenMain
        );
        assert_eq!(
            close_action(true, false, false, true, false, true),
            CloseAction::Close
        );
    }
}
