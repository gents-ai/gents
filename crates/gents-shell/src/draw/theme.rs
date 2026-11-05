use ratatui::style::{Color, Style};

use crate::sample::Tone;

pub const BG: Color = Color::from_u32(0x0f1411);
pub const FG: Color = Color::from_u32(0xc9d1cb);
pub const EDGE: Color = Color::from_u32(0x3a4a40);
pub const FOCUS: Color = Color::from_u32(0x7fc29b);
pub const TITLE: Color = Color::from_u32(0x8a9a90);
pub const LABEL: Color = Color::from_u32(0x7a897f);
pub const DIM: Color = Color::from_u32(0x333d37);
pub const ACCENT: Color = Color::from_u32(0x7fc29b);
pub const WARN: Color = Color::from_u32(0xe0b15e);
pub const ADD: Color = Color::from_u32(0x7fc29b);
pub const DEL: Color = Color::from_u32(0xd98880);
pub const SEL_BG: Color = Color::from_u32(0x283f2d);
pub const SEL_FG: Color = Color::from_u32(0xf2f5f0);
pub const TAB_BG: Color = Color::from_u32(0x1a221d);
pub const CHIP_FG: Color = Color::from_u32(0x0f1411);
pub const LOGO: Color = Color::from_u32(0xf2f5f0);
pub const MATCH: Color = WARN;
pub const YOU_BG: Color = Color::from_u32(0x22332a);

pub const SELECTED: Style = Style::new().fg(SEL_FG).bg(SEL_BG);
pub const TAB_OFF: Style = Style::new().fg(TITLE).bg(TAB_BG);

pub fn tone(tone: Tone) -> Style {
    let color = match tone {
        Tone::Plain => FG,
        Tone::Accent => ACCENT,
        Tone::Label => LABEL,
        Tone::Warn => WARN,
        Tone::Add => ADD,
        Tone::Del => DEL,
        Tone::You => FG,
        Tone::Reply => SEL_FG,
        Tone::File(_) => ACCENT,
    };
    Style::new().fg(color)
}

pub fn edge(focused: bool) -> Style {
    Style::new().fg(if focused { FOCUS } else { EDGE })
}
