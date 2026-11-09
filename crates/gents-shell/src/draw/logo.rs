use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::theme;

const GRID: [&str; 5] = ["...###.", "###....", ".....##", "..#....", "...###."];
const GRID_W: u16 = 14;
const GRID_H: u16 = 5;
const MAX_SCALE: u16 = 4;

pub fn logo_scale(free_w: u16, free_h: u16) -> u16 {
    let fit = (free_w / GRID_W)
        .min(free_h.saturating_sub(3) / GRID_H)
        .min(MAX_SCALE);
    if fit == 0 && free_h >= GRID_H && free_w >= GRID_W {
        1
    } else {
        fit
    }
}

pub fn draw(buf: &mut Buffer, free: Rect) {
    let scale = logo_scale(free.width, free.height);
    if scale == 0 {
        return;
    }
    let left = free.x + (free.width - GRID_W * scale) / 2;
    let top = free.y + (free.height - GRID_H * scale) / 2;
    for (row, line) in GRID.iter().enumerate() {
        for (col, cell) in line.chars().enumerate() {
            if cell == '#' {
                let block = Rect::new(
                    left + col as u16 * 2 * scale,
                    top + row as u16 * scale,
                    2 * scale,
                    scale,
                );
                buf.set_style(block, Style::new().bg(theme::LOGO));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::theme;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn logo_scale_matches_the_pictures() {
        assert_eq!(logo_scale(86, 34), 4); // 168x48
        assert_eq!(logo_scale(68, 22), 3); // 132x36
        assert_eq!(logo_scale(69, 16), 2); // 104x30
        assert_eq!(logo_scale(68, 10), 1); // 70x24
        assert_eq!(logo_scale(68, 7), 1); // still physically fits
        assert_eq!(logo_scale(68, 4), 0);
        assert_eq!(logo_scale(58, 2), 0); // 60x16
        assert_eq!(logo_scale(13, 22), 0);
    }

    #[test]
    fn does_not_paint_when_it_does_not_fit() {
        let area = Rect::new(0, 0, 13, 22);
        let mut buf = Buffer::empty(area);
        draw(&mut buf, area);
        assert!(buf.content().iter().all(|c| c.bg != theme::LOGO));
    }
}
