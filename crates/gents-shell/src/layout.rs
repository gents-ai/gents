use ratatui::layout::{Margin, Rect};

use crate::sample::MenuId;

pub const MIN_W: u16 = 60;
pub const KEYS_FULL_W: u16 = 84;
pub const MIN_H: u16 = 16;
pub const RAIL_W: u16 = 7;
const SESSIONS_MIN_W: u16 = 26;
const DOCK_W: u16 = 90;
const WIDE_W: u16 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wanted {
    pub sessions: bool,
    pub sidebar: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Right {
    None,
    Rail(Rect),
    Sidebar(Rect),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panes {
    pub sessions: Option<Rect>,
    pub agent: Rect,
    pub right: Right,
    pub status: Rect,
}

impl Panes {
    /// Everything above the status row, where layers float.
    pub fn above_status(&self) -> Rect {
        Rect::new(0, 0, self.status.width, self.status.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    TooSmall,
    Shell(Panes),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Jump,
    Picker,
    Menu(MenuId),
    Keys,
    Sessions,
    Sidebar,
}

pub fn sessions_can_dock(width: u16) -> bool {
    width >= DOCK_W
}

pub fn sidebar_can_dock(width: u16) -> bool {
    width >= WIDE_W
}

fn sessions_width(width: u16) -> u16 {
    let share = (21 * u32::from(width)).div_ceil(100) as u16;
    share.max(SESSIONS_MIN_W)
}

fn sidebar_width(width: u16) -> u16 {
    ((26 * u32::from(width) + 50) / 100) as u16
}

pub fn layout(width: u16, height: u16, wanted: Wanted) -> Screen {
    if width < MIN_W || height < MIN_H {
        return Screen::TooSmall;
    }
    let rows = height - 1;
    let pane = |x, w| Rect::new(x, 0, w, rows);
    let sessions_w = if wanted.sessions && sessions_can_dock(width) {
        sessions_width(width)
    } else {
        0
    };
    let (right, right_w) = if !wanted.sidebar || width < DOCK_W {
        (Right::None, 0)
    } else if sidebar_can_dock(width) {
        let w = sidebar_width(width);
        (Right::Sidebar(pane(width - w, w)), w)
    } else {
        (Right::Rail(pane(width - RAIL_W, RAIL_W)), RAIL_W)
    };
    Screen::Shell(Panes {
        sessions: (sessions_w > 0).then(|| pane(0, sessions_w)),
        agent: pane(sessions_w, width - sessions_w - right_w),
        right,
        status: Rect::new(0, rows, width, 1),
    })
}

pub fn conversation_area(pane: Rect) -> Rect {
    Rect::new(pane.x + 2, pane.y + 4, pane.width - 4, pane.height - 8)
}

pub fn running_area(pane: Rect, rows: u16) -> Rect {
    Rect::new(
        pane.x + 2,
        pane.y + pane.height - 4 - rows,
        pane.width - 4,
        rows,
    )
}

pub fn sidebar_rows_area(pane: Rect) -> Rect {
    let inner = pane.inner(Margin::new(1, 1));
    Rect {
        y: inner.y + 2,
        height: inner.height - 2,
        ..inner
    }
}

pub fn keys_rows_area(rect: Rect) -> Rect {
    let area = rect.inner(Margin::new(1, 1));
    Rect::new(area.x + 2, area.y + 3, area.width - 2, area.height - 5)
}

fn size_of(kind: LayerKind, on_top: bool) -> (u16, u16) {
    match kind {
        LayerKind::Jump if on_top => (72, 19),
        LayerKind::Jump => (40, 15),
        LayerKind::Picker => (66, 16),
        LayerKind::Menu(MenuId::NodeSettings) => (50, 23),
        LayerKind::Menu(_) => (50, 13),
        LayerKind::Keys => (KEYS_FULL_W, 24),
        LayerKind::Sessions => (36, 15),
        LayerKind::Sidebar => (40, 20),
    }
}

/// Each step of depth moves by (4, 2), and a window too small for that keeps the layer inside it.
pub fn layer_rect(kind: LayerKind, area: Rect, depth: u16, on_top: bool) -> Rect {
    let (want_w, want_h) = size_of(kind, on_top);
    let (w, h) = (want_w.min(area.width), want_h.min(area.height));
    let center_x = (area.width - w) / 2;
    let center_y = ((area.height - h) / 2).min(5);
    let (x, y) = match kind {
        LayerKind::Sessions => (3, 2),
        LayerKind::Picker => (center_x.saturating_sub(1), center_y + 2),
        LayerKind::Menu(_) => (center_x + 11, center_y + 3),
        LayerKind::Jump if !on_top => (center_x.saturating_sub(20), center_y.saturating_sub(1)),
        _ => (center_x, center_y),
    };
    Rect {
        x: x.saturating_add(depth.saturating_mul(4)),
        y: y.saturating_add(depth.saturating_mul(2)),
        width: w,
        height: h,
    }
    .clamp(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: Wanted = Wanted {
        sessions: true,
        sidebar: true,
    };
    const NO_SIDEBAR: Wanted = Wanted {
        sessions: true,
        sidebar: false,
    };
    const NO_SESSIONS: Wanted = Wanted {
        sessions: false,
        sidebar: true,
    };

    enum Side {
        Nothing,
        Rail(u16),
        Sidebar(u16),
    }

    /// Panes fill `height - 1` rows side by side, left to right, and status is the last row.
    fn check(
        width: u16,
        height: u16,
        wanted: Wanted,
        sessions: Option<u16>,
        agent: u16,
        side: Side,
    ) {
        let Screen::Shell(panes) = layout(width, height, wanted) else {
            panic!("{width}x{height} should fit");
        };
        let rows = height - 1;
        let agent_x = sessions.unwrap_or(0);
        let right_x = agent_x + agent;
        let right = match side {
            Side::Nothing => Right::None,
            Side::Rail(w) => Right::Rail(Rect::new(right_x, 0, w, rows)),
            Side::Sidebar(w) => Right::Sidebar(Rect::new(right_x, 0, w, rows)),
        };
        assert_eq!(panes.sessions, sessions.map(|w| Rect::new(0, 0, w, rows)));
        assert_eq!(panes.agent, Rect::new(agent_x, 0, agent, rows));
        assert_eq!(panes.right, right);
        assert_eq!(panes.status, Rect::new(0, rows, width, 1));
    }

    #[test]
    fn matches_the_pictures() {
        check(168, 48, ON, Some(36), 88, Side::Sidebar(44));
        check(132, 36, ON, Some(28), 70, Side::Sidebar(34));
        check(104, 30, ON, Some(26), 71, Side::Rail(7));
        check(70, 24, ON, None, 70, Side::Nothing);
    }

    #[test]
    fn docks_at_the_boundaries() {
        check(120, 30, ON, Some(26), 63, Side::Sidebar(31));
        check(119, 30, ON, Some(26), 86, Side::Rail(7));
        check(90, 30, ON, Some(26), 57, Side::Rail(7));
        check(89, 30, ON, None, 89, Side::Nothing);
        assert!(matches!(layout(60, 16, ON), Screen::Shell(_)));
        assert!(matches!(layout(59, 16, ON), Screen::TooSmall));
        assert!(matches!(layout(60, 15, ON), Screen::TooSmall));
        assert!(!sessions_can_dock(89) && sessions_can_dock(90));
        assert!(!sidebar_can_dock(119) && sidebar_can_dock(120));
    }

    #[test]
    fn follows_what_is_wanted() {
        check(132, 36, NO_SIDEBAR, Some(28), 104, Side::Nothing);
        check(132, 36, NO_SESSIONS, None, 98, Side::Sidebar(34));
        check(104, 30, NO_SIDEBAR, Some(26), 78, Side::Nothing);
        check(104, 30, NO_SESSIONS, None, 97, Side::Rail(7));
    }

    #[test]
    fn the_widest_window_does_not_overflow() {
        assert!(matches!(layout(u16::MAX, u16::MAX, ON), Screen::Shell(_)));
    }

    const KINDS: [LayerKind; 6] = [
        LayerKind::Jump,
        LayerKind::Picker,
        LayerKind::Menu(MenuId::Accounts),
        LayerKind::Keys,
        LayerKind::Sessions,
        LayerKind::Sidebar,
    ];

    #[test]
    fn every_layer_fits_every_size_and_depth() {
        for (w, h) in [(60, 16), (70, 24), (104, 30), (132, 36), (168, 48)] {
            let area = Rect::new(0, 0, w, h - 1);
            for depth in (0..20).chain([u16::MAX]) {
                for kind in KINDS {
                    for on_top in [true, false] {
                        let r = layer_rect(kind, area, depth, on_top);
                        assert_eq!(r.intersection(area), r, "{kind:?} {w}x{h} {depth}");
                        assert!(r.width > 0 && r.height > 0);
                    }
                }
            }
        }
    }

    #[test]
    fn positions_follow_the_pictures() {
        let area = Rect::new(0, 0, 132, 35);
        let accounts = LayerKind::Menu(MenuId::Accounts);
        assert_eq!(
            layer_rect(LayerKind::Keys, area, 0, true),
            Rect::new(24, 5, 84, 24)
        );
        assert_eq!(
            layer_rect(LayerKind::Jump, area, 0, true),
            Rect::new(30, 5, 72, 19)
        );
        assert_eq!(
            layer_rect(LayerKind::Picker, area, 0, true),
            Rect::new(32, 7, 66, 16)
        );
        assert_eq!(
            layer_rect(LayerKind::Jump, area, 0, false),
            Rect::new(26, 4, 40, 15)
        );
        assert_eq!(
            layer_rect(accounts, area, 1, true),
            Rect::new(56, 10, 50, 13)
        );
        assert_eq!(
            layer_rect(LayerKind::Sessions, Rect::new(0, 0, 70, 23), 0, true),
            Rect::new(3, 2, 36, 15)
        );
        let small = Rect::new(0, 0, 60, 15);
        assert_eq!(layer_rect(LayerKind::Keys, small, 0, true), small);
    }

    #[test]
    fn keys_table_has_room_for_every_row_when_the_layer_is_full_size() {
        let rows = keys_rows_area(Rect::new(24, 5, 84, 24));
        assert_eq!(rows, Rect::new(27, 9, 80, 17));
        assert_eq!(usize::from(rows.height), crate::sample::KEY_ROWS.len());
    }
}
