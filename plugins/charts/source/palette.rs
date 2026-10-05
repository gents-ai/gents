//! Colours: the light and dark themes, a colour-blind-safe categorical
//! palette (Okabe and Ito, 2008), and sequential and diverging ramps for
//! heatmaps. Every function is integer or basic float arithmetic, so a colour
//! is the same string on every platform.

use crate::err::{Res, fail};

/// An sRGB colour.
pub type Rgb = (u8, u8, u8);

/// The colours a chart is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// True for the dark theme.
    pub dark: bool,
    /// Page background.
    pub bg: &'static str,
    /// Primary text.
    pub fg: &'static str,
    /// Secondary text such as subtitles and tick labels.
    pub muted: &'static str,
    /// Grid lines.
    pub grid: &'static str,
    /// Axis lines and ticks.
    pub axis: &'static str,
}

/// The light theme.
pub const LIGHT: Theme = Theme {
    dark: false,
    bg: "#ffffff",
    fg: "#1f2328",
    muted: "#57606a",
    grid: "#e3e6ea",
    axis: "#6b7280",
};

/// The dark theme.
pub const DARK: Theme = Theme {
    dark: true,
    bg: "#14181d",
    fg: "#e6edf3",
    muted: "#a3adb8",
    grid: "#2b323a",
    axis: "#7d8794",
};

/// The theme named `name`: `light` or `dark`.
pub fn theme(name: &str) -> Res<Theme> {
    match name {
        "light" => Ok(LIGHT),
        "dark" => Ok(DARK),
        other => fail(format!("theme {other:?} is not known; use light or dark")),
    }
}

const SERIES_LIGHT: [&str; 8] = [
    "#0072b2", "#e69f00", "#009e73", "#d55e00", "#56b4e9", "#cc79a7", "#f0e442", "#999999",
];
const SERIES_DARK: [&str; 8] = [
    "#56b4e9", "#e69f00", "#009e73", "#f0e442", "#d55e00", "#cc79a7", "#3f8fd2", "#999999",
];

const VIRIDIS: [Rgb; 10] = [
    (0x44, 0x01, 0x54),
    (0x48, 0x28, 0x78),
    (0x3e, 0x4a, 0x89),
    (0x31, 0x68, 0x8e),
    (0x26, 0x82, 0x8e),
    (0x1f, 0x9e, 0x89),
    (0x35, 0xb7, 0x79),
    (0x6d, 0xcd, 0x59),
    (0xb4, 0xde, 0x2c),
    (0xfd, 0xe7, 0x25),
];

const DIVERGING_LIGHT: [Rgb; 7] = [
    (0x21, 0x66, 0xac),
    (0x43, 0x93, 0xc3),
    (0x92, 0xc5, 0xde),
    (0xf7, 0xf7, 0xf7),
    (0xf4, 0xa5, 0x82),
    (0xd6, 0x60, 0x4d),
    (0xb2, 0x18, 0x2b),
];
const DIVERGING_DARK: [Rgb; 7] = [
    (0x5a, 0xae, 0xe0),
    (0x3a, 0x82, 0xb8),
    (0x2c, 0x55, 0x77),
    (0x2b, 0x31, 0x39),
    (0x7a, 0x4a, 0x3a),
    (0xc5, 0x60, 0x3f),
    (0xf0, 0x8a, 0x5d),
];

/// `#rrggbb` for a colour.
pub fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Parses `#rgb` or `#rrggbb`.
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let digits = text.strip_prefix('#')?;
    if !digits.is_ascii() {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match digits.len() {
        3 => {
            let d = |i: usize| byte(&digits[i..=i]).map(|v| v * 17);
            Some((d(0)?, d(1)?, d(2)?))
        }
        6 => Some((
            byte(&digits[0..2])?,
            byte(&digits[2..4])?,
            byte(&digits[4..6])?,
        )),
        _ => None,
    }
}

/// Checks a caller palette and returns its colours as `#rrggbb`.
pub fn custom(colors: &[String]) -> Res<Vec<String>> {
    if colors.is_empty() || colors.len() > 64 {
        return fail("colors needs between 1 and 64 entries like \"#1f77b4\"");
    }
    colors
        .iter()
        .map(|c| match parse_hex(c.trim()) {
            Some(rgb) => Ok(hex(rgb)),
            None => fail(format!(
                "colors entry {c:?} is not a hex colour like \"#1f77b4\""
            )),
        })
        .collect()
}

fn mix(from: Rgb, to: Rgb, percent: u32) -> Rgb {
    let m =
        |a: u8, b: u8| ((u32::from(a) * (100 - percent) + u32::from(b) * percent + 50) / 100) as u8;
    (m(from.0, to.0), m(from.1, to.1), m(from.2, to.2))
}

/// Colour of series `index`: the base palette for the first eight, the same
/// hues lightened for the next eight and darkened for the eight after.
pub fn series_color(index: usize, theme: &Theme, custom: Option<&[String]>) -> String {
    if let Some(list) = custom {
        return list[index % list.len()].clone();
    }
    let base = if theme.dark {
        &SERIES_DARK
    } else {
        &SERIES_LIGHT
    };
    let rgb = parse_hex(base[index % 8]).unwrap_or((0, 0, 0));
    match (index / 8) % 3 {
        0 => hex(rgb),
        1 => hex(mix(rgb, (255, 255, 255), 40)),
        _ => hex(mix(rgb, (0, 0, 0), 35)),
    }
}

fn ramp(stops: &[Rgb], t: f64) -> Rgb {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let pos = t * (stops.len() - 1) as f64;
    let i = (pos.floor() as usize).min(stops.len() - 2);
    let f = pos - i as f64;
    let lerp = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * f).round() as u8;
    let (a, b) = (stops[i], stops[i + 1]);
    (lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

/// Sequential ramp (viridis) at `t` in 0..=1.
pub fn sequential(t: f64) -> Rgb {
    ramp(&VIRIDIS, t)
}

/// Diverging ramp (blue to red, neutral in the middle) at `t` in 0..=1.
pub fn diverging(t: f64, theme: &Theme) -> Rgb {
    ramp(
        if theme.dark {
            &DIVERGING_DARK
        } else {
            &DIVERGING_LIGHT
        },
        t,
    )
}

/// Black or white, whichever reads better on `rgb`.
pub fn readable_on(rgb: Rgb) -> &'static str {
    let luma = 299 * u32::from(rgb.0) + 587 * u32::from(rgb.1) + 114 * u32::from(rgb.2);
    if luma > 140_000 { "#111111" } else { "#ffffff" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_accepts_short_form() {
        assert_eq!(hex((0, 114, 178)), "#0072b2");
        assert_eq!(parse_hex("#0072B2"), Some((0, 114, 178)));
        assert_eq!(parse_hex("#fa0"), Some((255, 170, 0)));
        for bad in [
            "",
            "0072b2",
            "#12",
            "#12345",
            "#gggggg",
            "#1234567",
            "#\u{e9}\u{e9}\u{e9}",
        ] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn themes_are_found_by_name_and_unknown_names_fail_with_a_sentence() {
        assert!(!theme("light").unwrap().dark);
        assert!(theme("dark").unwrap().dark);
        assert_eq!(
            theme("blue").unwrap_err().0,
            "theme \"blue\" is not known; use light or dark"
        );
    }

    #[test]
    fn the_first_eight_series_colours_are_distinct_and_cycle_in_shades() {
        let all: Vec<String> = (0..24).map(|i| series_color(i, &LIGHT, None)).collect();
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), 24);
        assert_eq!(all[0], "#0072b2");
        assert_eq!(series_color(24, &LIGHT, None), all[0]);
    }

    #[test]
    fn dark_theme_uses_its_own_leading_colours() {
        assert_eq!(series_color(0, &DARK, None), "#56b4e9");
        assert_ne!(series_color(0, &DARK, None), series_color(0, &LIGHT, None));
    }

    #[test]
    fn a_custom_palette_wins_and_wraps() {
        let c = custom(&["#f00".into(), "#00FF00".into()]).unwrap();
        assert_eq!(c, ["#ff0000", "#00ff00"]);
        assert_eq!(series_color(2, &LIGHT, Some(&c)), "#ff0000");
    }

    #[test]
    fn a_bad_custom_palette_is_refused_with_the_entry() {
        assert!(custom(&[]).unwrap_err().0.contains("between 1 and 64"));
        assert!(custom(&["red".into()]).unwrap_err().0.contains("\"red\""));
        assert!(custom(&vec!["#fff".into(); 65]).is_err());
    }

    #[test]
    fn ramps_hit_their_end_stops_and_clamp() {
        assert_eq!(sequential(0.0), (0x44, 0x01, 0x54));
        assert_eq!(sequential(1.0), (0xfd, 0xe7, 0x25));
        assert_eq!(sequential(-3.0), sequential(0.0));
        assert_eq!(sequential(9.0), sequential(1.0));
        assert_eq!(sequential(f64::NAN), sequential(0.0));
        assert_eq!(diverging(0.5, &LIGHT), (0xf7, 0xf7, 0xf7));
        assert_eq!(diverging(0.0, &LIGHT), (0x21, 0x66, 0xac));
        assert_eq!(diverging(0.5, &DARK), (0x2b, 0x31, 0x39));
    }

    #[test]
    fn the_sequential_ramp_gets_lighter_along_its_length() {
        let luma = |c: Rgb| 299 * u32::from(c.0) + 587 * u32::from(c.1) + 114 * u32::from(c.2);
        let mut last = 0;
        for i in 0..=20 {
            let l = luma(sequential(f64::from(i) / 20.0));
            assert!(l >= last, "step {i}");
            last = l;
        }
    }

    #[test]
    fn readable_text_flips_between_dark_and_light_backgrounds() {
        assert_eq!(readable_on((255, 255, 255)), "#111111");
        assert_eq!(readable_on((0, 0, 0)), "#ffffff");
        assert_eq!(readable_on(sequential(0.0)), "#ffffff");
        assert_eq!(readable_on(sequential(1.0)), "#111111");
    }
}
