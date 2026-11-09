use gents::documents::EventSource;
use gents_shell::source::{Doc, FieldKind, Form, FormError, FormField};
use serde_json::{json, Value};

use super::{
    choice, delete, field, form_ref, merged, optional, problem, refused, shown, tags, tags_field,
    value, whole,
};
use crate::commands::shell::sections;

/// The only event kind the owner accepts (`event_trigger.rs:367-370`).
const CREATED: &str = "created";

/// The desktop's event source editor (`EventSourcesPanel.tsx:100-303`) on `current`, or on the
/// stub a new source starts from (`:357-374`) with `id`.
pub(super) fn form(current: Option<&Value>, id: &str) -> Form {
    let stub = json!({
        "event_source_id": id,
        "display_name": "New event source",
        "source_collection": "AgentRequest",
        "event_kind": CREATED,
    });
    let source = current.unwrap_or(&stub);
    let text = |key: &str, label: &str| field(key, label, FieldKind::Text, &shown(&source[key]));
    let group = &source["group"];
    let count = &group["expected_count"];
    let fixed = match count.is_number() {
        true => shown(count),
        false => String::new(),
    };
    let authorities = [
        ("", "None"),
        ("readOnly", "Read only"),
        ("readWrite", "Read / write"),
        ("integrate", "Integrate (inspect only)"),
    ];
    let authority = shown(&source["workspace_authority"]);
    let title = sections::title(Doc::EventSource, source);
    Form {
        form: form_ref(Doc::EventSource, current, id),
        title: title.clone(),
        note: Vec::new(),
        fields: vec![
            field("event_source_id", "Event source ID", FieldKind::Fixed, id),
            text("display_name", "Display name"),
            text("source_collection", "Source collection"),
            field("event_kind", "Event kind", FieldKind::Fixed, CREATED),
            text("filter", "Filter"),
            FormField {
                help: Some("Required when events are grouped.".to_string()),
                ..text("correlation_field", "Correlation field")
            },
            field(
                "workspace_authority",
                "Workspace authority",
                choice(authorities),
                &authority,
            ),
            field("grouping", "Grouping", FieldKind::Heading, ""),
            field("expected_count", "Expected count", FieldKind::Text, &fixed),
            field(
                "expected_count_field",
                "Expected count source field",
                FieldKind::Text,
                &shown(&count["source_field"]),
            ),
            field(
                "timeout_secs",
                "Timeout seconds",
                FieldKind::Text,
                &shown(&group["timeout_secs"]),
            ),
            field(
                "min_count",
                "Minimum count",
                FieldKind::Text,
                &shown(&group["min_count"]),
            ),
            field("metadata", "Metadata", FieldKind::Heading, ""),
            tags_field(source),
        ],
        actions: current
            .map(|_| delete(Doc::EventSource, title))
            .into_iter()
            .collect(),
    }
}

/// The event source `values` save as `id`, over `current`, as the desktop's editor writes it
/// (`EventSourcesPanel.tsx:136-172`). One count holds a fixed number or a source field, so both
/// typed is refused here; the owner checks how the rest of the group fits together.
pub(super) fn document(
    current: Option<&Value>,
    values: &[(String, String)],
    node_did: &str,
    id: &str,
) -> Result<EventSource, FormError> {
    let fixed = whole(values, "expected_count", "Expected count")?;
    let count_field = value(values, "expected_count_field").trim();
    if fixed.is_some() && !count_field.is_empty() {
        let both = "Choose a fixed expected count or a source field, not both";
        return Err(problem("expected_count_field", both));
    }
    let timeout = whole(values, "timeout_secs", "Timeout seconds")?;
    let min_count = whole(values, "min_count", "Minimum count")?;
    let count = match (fixed, count_field) {
        (Some(fixed), _) => json!(fixed),
        (None, "") => Value::Null,
        (None, count_field) => json!({ "source_field": count_field }),
    };
    let group = match count.is_null() && timeout.is_none() && min_count.is_none() {
        true => Value::Null,
        false => {
            json!({ "expected_count": count, "timeout_secs": timeout, "min_count": min_count })
        }
    };
    let fields = json!({
        "node_did": node_did,
        "event_source_id": id,
        "display_name": optional(value(values, "display_name")),
        "source_collection": value(values, "source_collection").trim(),
        "event_kind": CREATED,
        "filter": optional(value(values, "filter")),
        "correlation_field": optional(value(values, "correlation_field")),
        "group": group,
        "workspace_authority": optional(value(values, "workspace_authority")),
        "tags": tags(current, values),
    });
    merged(current, fields).map_err(refused)
}

#[cfg(test)]
mod tests {
    use gents::documents::EventGroupCount;
    use gents_shell::source::{FormAct, FormAction, FormRef};
    use serde_json::json;

    use super::*;
    use crate::commands::shell::testing::{keys_of, sent, shown_values};

    #[test]
    fn event_source_form_mirrors_the_desktop() {
        let leads = json!({
            "_docID": "bae-leads",
            "node_did": "owner",
            "event_source_id": "leads",
            "display_name": "Leads",
            "source_collection": "Lead",
            "event_kind": null,
            "filter": "{status: {_eq: \"open\"}}",
            "correlation_field": "batch_id",
            "group": { "expected_count": 3, "timeout_secs": 60, "min_count": 2 },
            "workspace_authority": "readOnly",
            "tags": ["gents:pack:crm", "sales"],
        });
        let sized = json!({
            "event_source_id": "sized",
            "display_name": null,
            "source_collection": "Lead",
            "group": { "expected_count": { "source_field": "size" } },
        });

        let edit = form(Some(&leads), "leads");
        let by_field = form(Some(&sized), "sized");
        let new = form(None, "evsrc-abc");

        assert_eq!(
            edit.form,
            FormRef::Edit(Doc::EventSource, "leads".to_string())
        );
        assert_eq!(edit.title, "Leads");
        assert_eq!(
            shown_values(&edit),
            [
                ("event_source_id", "leads"),
                ("display_name", "Leads"),
                ("source_collection", "Lead"),
                ("event_kind", "created"),
                ("filter", "{status: {_eq: \"open\"}}"),
                ("correlation_field", "batch_id"),
                ("workspace_authority", "readOnly"),
                ("grouping", ""),
                ("expected_count", "3"),
                ("expected_count_field", ""),
                ("timeout_secs", "60"),
                ("min_count", "2"),
                ("metadata", ""),
                ("tags", "sales"),
            ]
        );
        let fixed = ["event_source_id", "event_kind"];
        assert_eq!(keys_of(&edit, &FieldKind::Fixed), fixed);
        assert!(keys_of(&edit, &FieldKind::Lines).is_empty());
        let delete = FormAct::Delete {
            label: "Leads".to_string(),
            name: "Leads".to_string(),
        };
        let delete = FormAction {
            label: "Delete this event source".to_string(),
            act: delete,
        };
        assert_eq!(edit.actions, [delete]);

        assert_eq!(by_field.title, "sized");
        assert_eq!(
            shown_values(&by_field)[8..12],
            [
                ("expected_count", ""),
                ("expected_count_field", "size"),
                ("timeout_secs", ""),
                ("min_count", ""),
            ]
        );

        assert_eq!(new.form, FormRef::New(Doc::EventSource));
        assert_eq!(new.title, "New event source");
        assert_eq!(
            shown_values(&new)[..7],
            [
                ("event_source_id", "evsrc-abc"),
                ("display_name", "New event source"),
                ("source_collection", "AgentRequest"),
                ("event_kind", "created"),
                ("filter", ""),
                ("correlation_field", ""),
                ("workspace_authority", ""),
            ]
        );
        assert_eq!(new.actions, []);
    }

    #[test]
    fn a_group_is_written_from_its_counts() {
        let grouped = |pairs: &[(&str, &str)]| {
            let mut values = sent(&[
                ("source_collection", " Lead "),
                ("correlation_field", "batch_id"),
            ]);
            values.extend(sent(pairs));
            document(None, &values, "owner", "evsrc-a")
        };

        let problem = |field: &str, message: &str| {
            Err(FormError {
                field: Some(field.to_string()),
                message: message.to_string(),
            })
        };
        assert_eq!(
            grouped(&[("expected_count", "three"), ("timeout_secs", "x")]),
            problem("expected_count", "Expected count must be a whole number")
        );
        assert_eq!(
            grouped(&[("expected_count", "3"), ("expected_count_field", "size")]),
            problem(
                "expected_count_field",
                "Choose a fixed expected count or a source field, not both"
            )
        );
        assert_eq!(
            grouped(&[("min_count", "1.5")]),
            problem("min_count", "Minimum count must be a whole number")
        );
        let by_field = grouped(&[("expected_count_field", " size "), ("timeout_secs", "60")]);
        let by_field = by_field.unwrap();
        assert_eq!(by_field.source_collection, "Lead");
        assert_eq!(by_field.event_kind.as_deref(), Some("created"));
        let group = by_field.group.unwrap();
        let field = EventGroupCount::SourceField {
            source_field: "size".to_string(),
        };
        assert_eq!(group.expected_count, Some(field));
        assert_eq!((group.timeout_secs, group.min_count), (Some(60), None));
        assert_eq!(grouped(&[]).unwrap().group, None);
    }
}
