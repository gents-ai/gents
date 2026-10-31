use std::collections::BTreeMap;

use serde_json::Value;

use super::lookup::lookup;
use crate::eval::checks::captured_fields_match::{test, Expectation, FieldPath, Test};
use crate::eval::checks::{excerpt, EXCERPT_CHARS};

#[derive(Default)]
pub(super) struct Tally {
    pub(super) categories: BTreeMap<String, (usize, usize)>,
    pub(super) unmet: Vec<String>,
}

impl Tally {
    pub(super) fn record(
        &mut self,
        category: Option<&str>,
        met: bool,
        what: impl FnOnce() -> String,
    ) {
        let entry = self
            .categories
            .entry(category.unwrap_or("completeness").to_owned())
            .or_default();
        entry.1 += 1;
        if met {
            entry.0 += 1;
        } else {
            self.unmet.push(format!(
                "[{}] {}",
                category.unwrap_or("completeness"),
                what()
            ));
        }
    }

    pub(super) fn expectations(
        &mut self,
        category: Option<&str>,
        subject: &str,
        row: Option<&Value>,
        tests: &[(FieldPath, Test)],
    ) {
        for (field, test) in tests {
            let actual = row.and_then(|row| field.resolve(|path| Some(lookup(row, path))));
            let met = actual.as_ref().is_some_and(|actual| test.holds(actual));
            self.record(category, met, || {
                format!(
                    "{subject}: {field} expected {}, got {}",
                    excerpt(&test.describe(), EXCERPT_CHARS),
                    actual.map_or("no row".to_string(), |actual| excerpt(
                        &actual.to_string(),
                        EXCERPT_CHARS
                    ))
                )
            });
        }
    }
}

pub(super) fn tests(expectations: Vec<Expectation>) -> Result<Vec<(FieldPath, Test)>, String> {
    expectations.into_iter().map(test).collect()
}
