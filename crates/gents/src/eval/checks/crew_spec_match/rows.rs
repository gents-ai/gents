use serde_json::Value;

use super::params::{LinkSpec, Present, RowSpec};
use super::tally::{tests, Tally};
use super::{capture_rows, find_link, grader, CheckVerdict, StageEvidence};
use super::{find, lookup};
use crate::eval::checks::captured_fields_match::test;

pub(super) fn check_presence(
    specs: &[Present],
    stage: &StageEvidence,
    tally: &mut Tally,
) -> Result<(), CheckVerdict> {
    let rows = |name: &str| capture_rows(stage, name);
    for present in specs {
        if present
            .min
            .zip(present.max)
            .is_some_and(|(min, max)| min > max)
        {
            return Err(grader("bad_params", "capture minimum exceeds maximum"));
        }
        let count = rows(&present.capture).map(<[Value]>::len);
        tally.record(
            present.category.as_deref(),
            count.is_some_and(|n| {
                present.min.is_none_or(|min| n >= min) && present.max.is_none_or(|max| n <= max)
            }),
            || {
                format!(
                    "capture {} has {:?} rows; expected min {:?}, max {:?}",
                    present.capture, count, present.min, present.max
                )
            },
        );
    }
    Ok(())
}

pub(super) fn check_rows_and_links(
    row_specs: Vec<RowSpec>,
    link_specs: Vec<LinkSpec>,
    stage: &StageEvidence,
    tally: &mut Tally,
) -> Result<(), CheckVerdict> {
    let rows = |name: &str| capture_rows(stage, name);
    for spec in row_specs {
        let category = spec.category.as_deref();
        let tests = match tests(spec.expect) {
            Ok(tests) => tests,
            Err(detail) => return Err(grader("bad_params", detail)),
        };
        let row = rows(&spec.capture).and_then(|rows| find(rows, &spec.key, &spec.id));
        let subject = format!("{} {}", spec.capture, spec.id);
        tally.record(category, row.is_some(), || format!("{subject} absent"));
        tally.expectations(category, &subject, row, &tests);
    }
    for spec in link_specs {
        let tests = match tests(spec.expect) {
            Ok(tests) => tests,
            Err(detail) => return Err(grader("bad_params", detail)),
        };
        let source = if let Some(selector) = spec.source_match {
            if !spec.key.is_empty() || !spec.id.is_empty() {
                return Err(grader(
                    "bad_params",
                    "links use either key/id or source_match",
                ));
            }
            let (field, matcher) = match test(selector) {
                Ok(test) => test,
                Err(detail) => return Err(grader("bad_params", detail)),
            };
            rows(&spec.capture).and_then(|rows| {
                let mut matches = rows.iter().filter(|row| {
                    field
                        .resolve(|path| Some(lookup(row, path)))
                        .is_some_and(|actual| matcher.holds(&actual))
                });
                let first = matches.next();
                if matches.next().is_some() {
                    None
                } else {
                    first
                }
            })
        } else {
            if spec.key.is_empty() || spec.id.is_empty() {
                return Err(grader("bad_params", "links require key/id or source_match"));
            }
            rows(&spec.capture).and_then(|rows| find(rows, &spec.key, &spec.id))
        };
        let reference = source.map(|row| lookup(row, &spec.field));
        let mut target = reference.as_ref().and_then(Value::as_str).and_then(|id| {
            rows(&spec.target_capture)
                .and_then(|rows| find_link(rows, &spec.target_key, id, spec.exact))
        });
        for hop in &spec.via {
            let reference = target.map(|row| lookup(row, &hop.field));
            target = reference.as_ref().and_then(Value::as_str).and_then(|id| {
                rows(&hop.target_capture)
                    .and_then(|rows| find_link(rows, &hop.target_key, id, spec.exact))
            });
        }
        let (destination, destination_key) = spec
            .via
            .last()
            .map(|hop| (&hop.target_capture, &hop.target_key))
            .unwrap_or((&spec.target_capture, &spec.target_key));
        let source_name = if spec.id.is_empty() {
            source
                .map(|row| lookup(row, "display_name"))
                .unwrap_or(Value::Null)
                .to_string()
        } else {
            spec.id.clone()
        };
        let target_name = target
            .map(|row| lookup(row, destination_key))
            .unwrap_or(Value::Null);
        let subject = format!(
            "{} {} -> {} {}",
            spec.capture, source_name, destination, target_name
        );
        let category = spec.category.as_deref();
        tally.record(category, target.is_some(), || {
            format!("{subject}: unresolved reference")
        });
        tally.expectations(category, &subject, target, &tests);
        let final_message = stage
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|m| m.content.to_lowercase())
            .unwrap_or_default();
        for field in &spec.reported_fields {
            let value = target.map(|row| lookup(row, field)).unwrap_or(Value::Null);
            let text = match &value {
                Value::String(s) => s.to_lowercase(),
                other => other.to_string(),
            };
            tally.record(
                category,
                !value.is_null() && !text.is_empty() && final_message.contains(&text),
                || format!("final message does not report {subject}.{field}"),
            );
        }
    }
    Ok(())
}
