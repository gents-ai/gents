use std::collections::BTreeMap;
use std::f32::consts::TAU;

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals,
};

pub const FONT_UI_NAME: &str = "chakra-petch";
pub const FONT_MONO_NAME: &str = "space-mono";
pub const FONT_STENCIL_NAME: &str = "big-shoulders-stencil";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub background_0: Color32,
    pub background_1: Color32,
    pub background_2: Color32,
    pub background_3: Color32,
    pub background_4: Color32,
    pub stroke_subtle: Color32,
    pub stroke: Color32,
    pub stroke_strong: Color32,
    pub text_0: Color32,
    pub text_1: Color32,
    pub text_2: Color32,
    pub text_3: Color32,
    pub accent: Color32,
    pub accent_dim: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub info: Color32,
}

#[derive(Debug, Clone, Copy)]
pub struct ShellMetrics {
    pub activity_bar_width: f32,
    pub status_bar_height: f32,
    pub toolbar_height: f32,
    pub control_height: f32,
    pub section_spacing: f32,
}

pub fn palette() -> Palette {
    Palette {
        background_0: rgb(0x14, 0x11, 0x0D),
        background_1: rgb(0x1C, 0x18, 0x12),
        background_2: rgb(0x25, 0x1F, 0x17),
        background_3: rgb(0x2F, 0x28, 0x1E),
        background_4: rgb(0x3A, 0x31, 0x26),
        stroke_subtle: rgb(0x2A, 0x24, 0x1B),
        stroke: rgb(0x3C, 0x34, 0x2A),
        stroke_strong: rgb(0x55, 0x47, 0x36),
        text_0: rgb(0xE8, 0xDC, 0xC7),
        text_1: rgb(0xB3, 0xA0, 0x85),
        text_2: rgb(0x7D, 0x6C, 0x55),
        text_3: rgb(0x4D, 0x43, 0x37),
        accent: rgb(0xD1, 0x7A, 0x3A),
        accent_dim: rgb(0x8A, 0x4F, 0x25),
        warning: rgb(0xE8, 0xA8, 0x5A),
        danger: rgb(0xB8, 0x55, 0x40),
        info: rgb(0x5E, 0x82, 0x82),
    }
}

pub fn metrics() -> ShellMetrics {
    ShellMetrics {
        activity_bar_width: 52.0,
        status_bar_height: 26.0,
        toolbar_height: 42.0,
        control_height: 36.0,
        section_spacing: 12.0,
    }
}

pub fn stencil_family() -> FontFamily {
    FontFamily::Name(FONT_STENCIL_NAME.into())
}

pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        FONT_UI_NAME.into(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/fonts/ChakraPetch-Regular.ttf"
        )))
        .into(),
    );
    fonts.font_data.insert(
        FONT_MONO_NAME.into(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/fonts/SpaceMono-Regular.ttf"
        )))
        .into(),
    );
    fonts.font_data.insert(
        FONT_STENCIL_NAME.into(),
        FontData::from_static(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/fonts/BigShouldersStencilDisplay-Regular.ttf"
        )))
        .into(),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, FONT_UI_NAME.into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, FONT_MONO_NAME.into());
    fonts
        .families
        .insert(stencil_family(), vec![FONT_STENCIL_NAME.into()]);

    fonts
}

pub fn text_styles() -> BTreeMap<TextStyle, FontId> {
    BTreeMap::from([
        (TextStyle::Heading, FontId::new(25.0, stencil_family())),
        (
            TextStyle::Name("title".into()),
            FontId::new(18.0, stencil_family()),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.0, FontFamily::Monospace),
        ),
        (TextStyle::Small, FontId::new(11.0, FontFamily::Monospace)),
    ])
}

pub fn apply_theme(ctx: &egui::Context) {
    let palette = palette();

    ctx.set_fonts(font_definitions());

    let mut style = (*ctx.global_style()).clone();
    style.text_styles = text_styles();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.indent = 14.0;
    style.visuals = visuals();

    ctx.set_global_style(style);

    let mut visuals = ctx.global_style().visuals.clone();
    visuals.override_text_color = Some(palette.text_0);
    ctx.set_visuals(visuals);
}

pub fn visuals() -> Visuals {
    let palette = palette();
    let mut visuals = Visuals::dark();

    visuals.override_text_color = Some(palette.text_0);
    visuals.panel_fill = palette.background_1;
    visuals.window_fill = palette.background_1;
    visuals.extreme_bg_color = palette.background_2;
    visuals.faint_bg_color = palette.background_1;
    visuals.code_bg_color = palette.background_2;
    visuals.selection.bg_fill = palette.accent_dim;
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.hyperlink_color = palette.info;
    visuals.warn_fg_color = palette.warning;
    visuals.error_fg_color = palette.danger;

    set_widget_visuals(
        &mut visuals.widgets.noninteractive,
        palette.background_1,
        palette.background_1,
        palette.stroke_subtle,
        palette.text_1,
    );
    set_widget_visuals(
        &mut visuals.widgets.inactive,
        palette.background_2,
        palette.background_2,
        palette.stroke,
        palette.text_1,
    );
    set_widget_visuals(
        &mut visuals.widgets.hovered,
        palette.background_2,
        palette.background_3,
        palette.stroke_strong,
        palette.text_0,
    );
    set_widget_visuals(
        &mut visuals.widgets.active,
        palette.background_3,
        palette.background_3,
        palette.accent_dim,
        palette.text_0,
    );
    set_widget_visuals(
        &mut visuals.widgets.open,
        palette.background_2,
        palette.background_3,
        palette.accent_dim,
        palette.text_0,
    );

    visuals
}

pub fn throb_color(ctx: &egui::Context, base: Color32) -> Color32 {
    let phase = ctx.input(|input| input.time) as f32;
    let intensity = 0.72 + 0.28 * (0.5 + 0.5 * (phase * TAU / 3.6).sin());
    base.gamma_multiply(intensity)
}

fn set_widget_visuals(
    visuals: &mut egui::style::WidgetVisuals,
    fill: Color32,
    weak_fill: Color32,
    stroke_color: Color32,
    text_color: Color32,
) {
    visuals.bg_fill = fill;
    visuals.weak_bg_fill = weak_fill;
    visuals.bg_stroke = Stroke::new(1.0, stroke_color);
    visuals.fg_stroke = Stroke::new(1.0, text_color);
}

const fn rgb(red: u8, green: u8, blue: u8) -> Color32 {
    Color32::from_rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_color_matches_spec() {
        assert_eq!(palette().accent, Color32::from_rgb(0xD1, 0x7A, 0x3A));
    }

    #[test]
    fn font_mapping_prefers_expected_families() {
        let fonts = font_definitions();

        assert_eq!(
            fonts.families[&FontFamily::Proportional][0],
            FONT_UI_NAME.to_owned()
        );
        assert_eq!(
            fonts.families[&FontFamily::Monospace][0],
            FONT_MONO_NAME.to_owned()
        );
        assert_eq!(text_styles()[&TextStyle::Heading].family, stencil_family());
    }
}
