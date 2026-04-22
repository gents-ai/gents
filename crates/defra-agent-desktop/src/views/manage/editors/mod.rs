mod render;

use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::audit;
use crate::state::{
    BackendDraft, BehaviorDraft, InferenceProfileDraft, ScheduleDraft, TaskDraft,
    ToolSelectionDraft,
};
use crate::theme;

pub(super) fn render_behavior_editor(ui: &mut Ui, draft: &mut BehaviorDraft) {
    render::render_behavior_editor(ui, draft);
}

pub(super) fn render_backend_editor(ui: &mut Ui, draft: &mut BackendDraft) {
    render::render_backend_editor(ui, draft);
}

pub(super) fn render_tool_selection_editor(ui: &mut Ui, draft: &mut ToolSelectionDraft) {
    render::render_tool_selection_editor(ui, draft);
}

pub(super) fn render_inference_profile_editor(ui: &mut Ui, draft: &mut InferenceProfileDraft) {
    render::render_inference_profile_editor(ui, draft);
}

pub(super) fn render_task_editor(ui: &mut Ui, draft: &mut TaskDraft) {
    render::render_task_editor(ui, draft);
}

pub(super) fn render_schedule_editor(ui: &mut Ui, draft: &mut ScheduleDraft) {
    render::render_schedule_editor(ui, draft);
}

pub(super) fn editor_heading(ui: &mut Ui, title: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(title)
            .family(theme::stencil_family())
            .size(13.0)
            .color(palette.text_1)
            .strong(),
    );
    ui.add_space(8.0);
}

pub(super) fn read_only_field(ui: &mut Ui, label: &str, value: &str) {
    let palette = theme::palette();
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    ui.label(
        RichText::new(if value.trim().is_empty() {
            "unset"
        } else {
            value
        })
        .monospace()
        .size(10.5)
        .color(palette.text_1),
    );
    ui.add_space(6.0);
}

pub(super) fn read_only_multiline(ui: &mut Ui, label: &str, value: &str, rows: usize) {
    let mut value = if value.trim().is_empty() {
        "unset".to_string()
    } else {
        value.to_string()
    };
    let palette = theme::palette();
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    ui.add_sized(
        [ui.available_width(), rows as f32 * 18.0 + 12.0],
        TextEdit::multiline(&mut value)
            .desired_rows(rows)
            .interactive(false),
    );
    ui.add_space(6.0);
}

pub(super) fn text_field(ui: &mut Ui, label: &str, value: &mut String) {
    let palette = theme::palette();
    let target = audit::targets::manage_field(label);
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    audit::add(
        ui,
        &target,
        TextEdit::singleline(value)
            .id_source(&target)
            .desired_width(ui.available_width()),
    );
    ui.add_space(6.0);
}

pub(super) fn multiline_field(ui: &mut Ui, label: &str, value: &mut String, rows: usize) {
    let palette = theme::palette();
    let target = audit::targets::manage_field(label);
    ui.label(
        RichText::new(label)
            .monospace()
            .size(10.5)
            .color(palette.text_2),
    );
    audit::add_sized(
        ui,
        &target,
        [ui.available_width(), rows as f32 * 18.0 + 12.0],
        TextEdit::multiline(value)
            .id_source(&target)
            .desired_rows(rows),
    );
    ui.add_space(6.0);
}

pub(super) fn toggle_field(ui: &mut Ui, label: &str, value: &mut bool) {
    let target = audit::targets::manage_toggle(label);
    audit::add(ui, &target, egui::Checkbox::new(value, label));
    ui.add_space(6.0);
}
