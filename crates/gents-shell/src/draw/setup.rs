use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::fields::{self, draw_fields, label_width, Status};
use super::layers::list_start;
use super::{draw_footer, draw_row, pane_box, theme};
use crate::app::fields::Field;
use crate::app::setup::{Setup, Step};
use crate::app::App;
use crate::sample::Tone;
use crate::source::SetupAuth;

const WAITING: &str = " Waiting for you to sign in in your browser.";
const LABEL_W: usize = 10;
const SETTING_W: u16 = 18;

pub fn draw(app: &App, rect: Rect, focused: bool, buf: &mut Buffer) {
    let Some(setup) = &app.setup else {
        return;
    };
    pane_box(buf, rect, &title(setup), focused);
    let area = rect.inner(Margin::new(1, 1));
    let status = Status::new(setup.status.as_ref(), area);
    let list = Rect {
        height: area.height.saturating_sub(2 + status.rows),
        ..area
    };
    match setup.step {
        Step::Provider => {
            let rows: Vec<String> = setup
                .providers
                .iter()
                .map(|provider| format!(" {}   {}", provider.name, provider.about))
                .collect();
            draw_list(buf, list, &rows, setup.provider);
        }
        Step::Method => {
            let rows: Vec<String> = setup
                .choices()
                .into_iter()
                .map(|(method, saved)| match (method.auth, saved) {
                    (_, Some(account)) => format!(" {} · use {}", method.name, account.label),
                    (SetupAuth::SignIn, None) => format!(" {} · new account", method.name),
                    _ => format!(" {}", method.name),
                })
                .collect();
            draw_list(buf, list, &rows, setup.choice);
        }
        Step::Connect | Step::Models | Step::Settings | Step::Saving => {
            if let Some((fields, _, width)) = form(setup) {
                draw_fields(fields, width, list, buf);
            }
            if setup.step == Step::Models {
                let rows: Vec<String> = setup
                    .matches()
                    .into_iter()
                    .map(|model| {
                        if model.name == model.id {
                            format!(" {}", model.id)
                        } else {
                            format!(" {}   {}", model.name, model.id)
                        }
                    })
                    .collect();
                let below = Rect {
                    y: list.y + 1,
                    height: list.height.saturating_sub(1),
                    ..list
                };
                draw_list(buf, below, &rows, setup.model);
            }
        }
        Step::SigningIn => {
            buf.set_stringn(
                list.x,
                list.y,
                WAITING,
                usize::from(list.width),
                theme::tone(Tone::Plain),
            );
            if let Some(url) = &setup.url {
                let below = Rect {
                    y: list.y + 2,
                    height: list.height.saturating_sub(2),
                    ..list
                };
                Paragraph::new(format!(" {url}"))
                    .style(theme::tone(Tone::Accent))
                    .wrap(Wrap { trim: false })
                    .render(below, buf);
            }
        }
    }
    status.draw(area, buf);
    draw_footer(
        app,
        area,
        area.bottom().saturating_sub(1),
        footer(setup),
        buf,
    );
}

fn title(setup: &Setup) -> String {
    let provider = setup
        .chosen_provider()
        .map_or("", |provider| &provider.name);
    match setup.step {
        Step::Provider => "Add a provider".to_string(),
        Step::Method => format!("{provider}: connection"),
        Step::Connect | Step::SigningIn => {
            let method = setup.choice().map_or("", |(method, _)| &method.name);
            format!("{provider}: {method}")
        }
        Step::Models => "Choose a model".to_string(),
        Step::Settings | Step::Saving => setup
            .chosen
            .as_ref()
            .map_or_else(String::new, |model| model.id.clone()),
    }
}

fn footer(setup: &Setup) -> &'static str {
    let signs_in = setup
        .choice()
        .is_some_and(|(method, _)| method.auth == SetupAuth::SignIn);
    match setup.step {
        Step::Provider => " enter choose · esc close",
        Step::Method => " enter choose · esc back",
        Step::Connect if signs_in => " enter sign in · esc back",
        Step::Connect => " enter connect · tab next field · esc back",
        Step::SigningIn => " esc cancel",
        Step::Models => " type to search · enter choose · esc back",
        Step::Settings => " enter save · tab next · ←→ choose · esc back",
        // A save cannot be called back.
        Step::Saving => "",
    }
}

fn draw_list(buf: &mut Buffer, list: Rect, rows: &[String], selected: usize) {
    let first = list_start(selected, usize::from(list.height));
    for (index, y) in (first..rows.len()).zip(list.top()..list.bottom()) {
        draw_row(buf, list, y, &rows[index], Tone::Plain, index == selected);
    }
}

/// The fields a step shows, the focused one, and their label width.
fn form(setup: &Setup) -> Option<(&[Field], usize, u16)> {
    match setup.step {
        Step::Connect => {
            let width = label_width(&setup.fields, LABEL_W);
            Some((&setup.fields, setup.focus, width))
        }
        Step::Models if setup.shows_query() => {
            let width = setup.query.label.len() as u16 + 2;
            Some((std::slice::from_ref(&setup.query), 0, width))
        }
        Step::Settings | Step::Saving => Some((&setup.settings, setup.setting, SETTING_W)),
        _ => None,
    }
}

/// On the focused field of a step that takes keys.
pub fn cursor(setup: &Setup, rect: Rect) -> Option<Position> {
    if setup.step == Step::Saving {
        return None;
    }
    let (fields, focus, width) = form(setup)?;
    let field = fields.get(focus)?;
    let area = rect.inner(Margin::new(1, 1));
    let y = area.y + focus as u16;
    Some(fields::cursor(field, width, Rect { y, ..area }))
}
