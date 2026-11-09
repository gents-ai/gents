use gents::documents::Schedule;
use gents_shell::source::{Doc, FieldKind, Form, FormAct, FormAction, FormError, FormField};
use serde_json::{json, Value};

use super::{
    choice, delete, field, form_ref, merged, optional, problem, refused, shown, shown_when, tags,
    tags_field, value, whole,
};
use crate::commands::shell::sections;

const INTERVAL: &str = "Interval seconds";

/// The desktop's schedule editor (`SchedulesPanel.tsx:40-223`) on `current`, or on the stub a new
/// schedule starts from (`:277-293`) with `id`. A saved schedule can be run now.
pub(super) fn form(current: Option<&Value>, id: &str) -> Form {
    let stub = json!({
        "schedule_id": id,
        "display_name": "New schedule",
        "cadence": { "kind": "cron", "expression": "0 * * * *", "timezone": "UTC" },
    });
    let schedule = current.unwrap_or(&stub);
    let cadence = &schedule["cadence"];
    let (kind, interval, expression, timezone) = match cadence["kind"] == "interval" {
        true => ("interval", shown(&cadence["interval_secs"]), "", "UTC"),
        false => (
            "cron",
            String::new(),
            cadence["expression"].as_str().unwrap_or_default(),
            cadence["timezone"].as_str().unwrap_or_default(),
        ),
    };
    let on = |field, cadence| shown_when(field, "cadence", cadence);
    let cadences = [("interval", "Interval"), ("cron", "Cron")];
    let seconds = FormField {
        help: Some("Positive whole number.".to_string()),
        ..field("interval_secs", INTERVAL, FieldKind::Text, &interval)
    };
    let display_name = shown(&schedule["display_name"]);
    let title = sections::title(Doc::Schedule, schedule);
    let mut fields = vec![
        field("schedule_id", "Schedule ID", FieldKind::Fixed, id),
        field(
            "display_name",
            "Display name",
            FieldKind::Text,
            &display_name,
        ),
        field("cadence", "Cadence", choice(cadences), kind),
        on(seconds, "interval"),
        on(
            field("expression", "Cron", FieldKind::Text, expression),
            "cron",
        ),
        on(
            field("timezone", "Timezone", FieldKind::Text, timezone),
            "cron",
        ),
        tags_field(schedule),
    ];
    let mut actions = Vec::new();
    if current.is_some() {
        fields.push(FormField {
            help: Some("Uses the enabled trigger currently bound to this schedule.".to_string()),
            ..field("manual_run", "Manual run", FieldKind::Heading, "")
        });
        actions.push(FormAction {
            label: "Run schedule now".to_string(),
            act: FormAct::RunSchedule,
        });
        actions.push(delete(Doc::Schedule, title.clone()));
    }
    Form {
        form: form_ref(Doc::Schedule, current, id),
        title,
        note: Vec::new(),
        fields,
        actions,
    }
}

/// The schedule `values` save as `id`, over `current`, as the desktop's editor writes it
/// (`SchedulesPanel.tsx:66-88`): a cron schedule misses runs latest only.
pub(super) fn document(
    current: Option<&Value>,
    values: &[(String, String)],
    node_did: &str,
    id: &str,
) -> Result<Schedule, FormError> {
    let cadence = match value(values, "cadence") {
        "interval" => {
            let secs = whole(values, "interval_secs", INTERVAL)?;
            let required = || problem("interval_secs", &format!("{INTERVAL} is required"));
            json!({ "kind": "interval", "interval_secs": secs.ok_or_else(required)? })
        }
        _ => json!({
            "kind": "cron",
            "expression": value(values, "expression").trim(),
            "timezone": value(values, "timezone").trim(),
            "missed_run_policy": "latest_only",
        }),
    };
    let fields = json!({
        "node_did": node_did,
        "schedule_id": id,
        "display_name": optional(value(values, "display_name")),
        "cadence": cadence,
        "tags": tags(current, values),
    });
    merged(current, fields).map_err(refused)
}

#[cfg(test)]
mod tests {
    use gents::documents::schedule_cron::CronMissedRunPolicy;
    use gents::documents::ScheduleCadence;
    use gents_shell::source::{FieldKind, FormAct, FormAction, FormField, FormRef};
    use serde_json::json;

    use super::super::testing::{sent, shown_values};
    use super::super::{field, hinted};
    use super::*;

    fn shown_on(field: FormField, cadence: &str) -> FormField {
        shown_when(field, "cadence", cadence)
    }

    #[test]
    fn schedule_form_mirrors_the_desktop() {
        let daily = json!({
            "_docID": "bae-daily",
            "node_did": "owner",
            "schedule_id": "daily",
            "display_name": "Daily",
            "cadence": { "kind": "cron", "expression": "0 2 * * *", "timezone": "Europe/Paris" },
            "tags": ["gents:pack:ops", "nightly"],
        });
        let often = json!({
            "schedule_id": "often",
            "display_name": null,
            "cadence": { "kind": "interval", "interval_secs": 900 },
        });

        let edit = form(Some(&daily), "daily");
        let interval = form(Some(&often), "often");
        let new = form(None, "sched-abc");

        assert_eq!(edit.form, FormRef::Edit(Doc::Schedule, "daily".to_string()));
        assert_eq!(edit.title, "Daily");
        assert_eq!(edit.note, Vec::<String>::new());
        let cadences = [("interval", "Interval"), ("cron", "Cron")];
        let seconds = field("interval_secs", "Interval seconds", FieldKind::Text, "");
        let manual = field("manual_run", "Manual run", FieldKind::Heading, "");
        assert_eq!(
            edit.fields,
            [
                field("schedule_id", "Schedule ID", FieldKind::Fixed, "daily"),
                field("display_name", "Display name", FieldKind::Text, "Daily"),
                field("cadence", "Cadence", choice(cadences), "cron"),
                shown_on(
                    FormField {
                        help: Some("Positive whole number.".to_string()),
                        ..seconds
                    },
                    "interval"
                ),
                shown_on(
                    field("expression", "Cron", FieldKind::Text, "0 2 * * *"),
                    "cron"
                ),
                shown_on(
                    field("timezone", "Timezone", FieldKind::Text, "Europe/Paris"),
                    "cron"
                ),
                hinted(
                    field("tags", "Tags", FieldKind::Text, "nightly"),
                    "Add a tag"
                ),
                FormField {
                    help: Some(
                        "Uses the enabled trigger currently bound to this schedule.".to_string()
                    ),
                    ..manual
                },
            ]
        );
        let delete = FormAct::Delete {
            label: "Daily".to_string(),
            name: "Daily".to_string(),
        };
        let actions = [
            ("Run schedule now", FormAct::RunSchedule),
            ("Delete this schedule", delete),
        ];
        let actions = actions.map(|(label, act)| FormAction {
            label: label.to_string(),
            act,
        });
        assert_eq!(edit.actions, actions);

        assert_eq!(interval.title, "often");
        assert_eq!(
            shown_values(&interval)[..6],
            [
                ("schedule_id", "often"),
                ("display_name", ""),
                ("cadence", "interval"),
                ("interval_secs", "900"),
                ("expression", ""),
                ("timezone", "UTC"),
            ]
        );

        assert_eq!(new.form, FormRef::New(Doc::Schedule));
        assert_eq!(new.title, "New schedule");
        assert_eq!(
            shown_values(&new),
            [
                ("schedule_id", "sched-abc"),
                ("display_name", "New schedule"),
                ("cadence", "cron"),
                ("interval_secs", ""),
                ("expression", "0 * * * *"),
                ("timezone", "UTC"),
                ("tags", ""),
            ]
        );
        assert_eq!(new.actions, []);
    }

    #[test]
    fn an_interval_must_be_a_whole_number() {
        let every = |secs: &str| sent(&[("cadence", "interval"), ("interval_secs", secs)]);
        let saved = |secs: &str| document(None, &every(secs), "owner", "sched-a");

        let problem = |message: &str| {
            Err(FormError {
                field: Some("interval_secs".to_string()),
                message: message.to_string(),
            })
        };
        assert_eq!(
            saved("ten"),
            problem("Interval seconds must be a whole number")
        );
        assert_eq!(saved(" "), problem("Interval seconds is required"));
        let cadence = |secs: &str| saved(secs).map(|schedule| schedule.cadence);
        assert_eq!(
            cadence(" 900 "),
            Ok(ScheduleCadence::Interval { interval_secs: 900 })
        );
        assert_eq!(
            cadence("0"),
            Ok(ScheduleCadence::Interval { interval_secs: 0 }),
            "the owner checks the bound"
        );
    }

    #[test]
    fn a_cron_schedule_misses_runs_latest_only() {
        let often = json!({
            "node_did": "owner",
            "schedule_id": "often",
            "display_name": "Often",
            "cadence": { "kind": "interval", "interval_secs": 900 },
            "created_at": "2026-01-01T00:00:00Z",
            "tags": ["gents:pack:ops"],
        });
        let values = sent(&[
            ("display_name", " Mornings "),
            ("cadence", "cron"),
            ("expression", " 0 9 * * * "),
            ("timezone", " Europe/Paris "),
            ("tags", "daily"),
        ]);

        let saved = document(Some(&often), &values, "owner", "often").unwrap();

        assert_eq!(
            saved.cadence,
            ScheduleCadence::Cron {
                expression: "0 9 * * *".to_string(),
                timezone: "Europe/Paris".to_string(),
                missed_run_policy: Some(CronMissedRunPolicy::LatestOnly),
            }
        );
        assert_eq!(saved.display_name.as_deref(), Some("Mornings"));
        assert_eq!(saved.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(saved.tags, ["gents:pack:ops", "daily"]);
    }
}
