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
        self.expectations_impl(category, subject, row, tests, false);
    }

    pub(super) fn tools_expectations(
        &mut self,
        category: Option<&str>,
        subject: &str,
        row: Option<&Value>,
        tests: &[(FieldPath, Test)],
    ) {
        self.expectations_impl(category, subject, row, tests, true);
    }

    fn expectations_impl(
        &mut self,
        category: Option<&str>,
        subject: &str,
        row: Option<&Value>,
        tests: &[(FieldPath, Test)],
        tools: bool,
    ) {
        for (field, test) in tests {
            let actual = row.and_then(|row| field.resolve(|path| Some(lookup(row, path))));
            let met = actual.as_ref().is_some_and(|actual| match test {
                Test::Equals(expected)
                    if tools
                        && field.is_exact("self_config.self_config_categories")
                        && expected.is_array() =>
                {
                    configured_categories_equal(actual, expected)
                }
                _ => test.holds(actual),
            });
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

fn configured_categories_equal(actual: &Value, expected: &Value) -> bool {
    let parse = |value: &Value| serde_json::from_value::<Vec<String>>(value.clone()).ok();
    match (parse(actual), parse(expected)) {
        (Some(actual), Some(expected)) => {
            crate::tools::surface::configured_self_config_category_set(&actual)
                == crate::tools::surface::configured_self_config_category_set(&expected)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_tools_self_config_category_equality_uses_runtime_set_semantics() {
        let cases = [
            (json!(["node", "agent"]), json!(["agent", "node"]), true),
            (
                json!(["agent", "node", "automation"]),
                json!(["agent", "node"]),
                false,
            ),
            (json!(["agent"]), json!(["agent", "node"]), false),
            (json!(["agent", 42]), json!(["agent", "node"]), false),
        ];
        for (actual, expected, equal) in cases {
            assert_eq!(configured_categories_equal(&actual, &expected), equal);
        }
        let expectations: Vec<Expectation> = serde_json::from_value(json!([
            {"field":"self_config.self_config_categories", "equals":["agent", "node"]},
            {"field":"other_array", "equals":["agent", "node"]}
        ]))
        .unwrap();
        let tests = tests(expectations).unwrap();
        let row = json!({"self_config":{"self_config_categories":["node", "agent"]},
            "other_array":["node", "agent"]});
        let mut tally = Tally::default();
        tally.tools_expectations(None, "agent tools", Some(&row), &tests);
        assert_eq!(tally.categories["completeness"], (1, 2));
        assert_eq!(tally.unmet.len(), 1);
        assert!(tally.unmet[0].contains("other_array"));
        let mut generic = Tally::default();
        generic.expectations(None, "generic", Some(&row), &tests);
        assert_eq!(generic.categories["completeness"], (0, 2));
    }
}
