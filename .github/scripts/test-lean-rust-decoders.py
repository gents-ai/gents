#!/usr/bin/env python3
"""Negative controls for the Lean/Rust fixture structural lint."""
import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "decoders", Path(__file__).with_name("check-lean-rust-decoders.py"))
decoders = importlib.util.module_from_spec(spec)
spec.loader.exec_module(decoders)

RUST = '''
pub(crate) use gents_protocol::request_lifecycle::RequestLifecycleState as Phase;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Case<T> {
    /// A doc comment, with commas, (and brackets) that must not corrupt parsing.
    pub(crate) name: String,
    pub(crate) phase: Phase,
    pub(crate) owner: (u64, u64),
    pub(crate) payload: T,
    pub(crate) deadline: Option<u64>,
    #[serde(default)]
    pub(crate) notes: Vec<String>,
    pub(crate) steps: Vec<Step>,
    pub(crate) outcome: Outcome,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Step {
    /// Renewal, which never follows from output.
    RenewLease { now: u64 },
    Release,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Complete,
    Partial,
}
'''
GROUPS = {"cases": "Case<bool>"}
VALID = {"cases": [{
    "name": "ok", "phase": "processing", "owner": [1, 2], "payload": True,
    "deadline": None, "steps": [{"operation": "renew_lease", "now": 5},
                                {"operation": "release"}],
    "outcome": "partial"}]}


class DecoderTests(unittest.TestCase):
    def setUp(self):
        self.items = decoders.parse_items(RUST)

    def errors(self, mutate=None):
        payload = copy.deepcopy(VALID)
        if mutate:
            mutate(payload["cases"][0])
        return decoders.validate(self.items, payload, GROUPS)

    def test_valid_payload_including_omitted_default_and_option(self):
        self.assertEqual(self.errors(), [])
        self.assertEqual(self.errors(lambda case: case.pop("deadline")), [])

    def test_required_nullable_accepts_null_but_rejects_missing(self):
        self.items = decoders.parse_items(RUST.replace(
            "pub(crate) deadline: Option<u64>,",
            '#[serde(deserialize_with = "crate::lean_vocab_test::required_nullable")]\n'
            "    pub(crate) deadline: Option<u64>,"))
        self.assertEqual(self.errors(), [])
        self.assertEqual(self.errors(lambda case: case.__setitem__("deadline", 5)), [])
        self.assertIn("missing field `deadline`", "\n".join(
            self.errors(lambda case: case.pop("deadline"))))
        self.assertIn("expected u64", "\n".join(
            self.errors(lambda case: case.__setitem__("deadline", "5"))))

    def test_relative_required_nullable_accepts_null_but_rejects_missing(self):
        self.items = decoders.parse_items(RUST.replace(
            "pub(crate) deadline: Option<u64>,",
            '#[serde(deserialize_with = "required_nullable")]\n'
            "    pub(crate) deadline: Option<u64>,"))
        self.assertEqual(self.errors(), [])
        self.assertIn("missing field `deadline`", "\n".join(
            self.errors(lambda case: case.pop("deadline"))))

    def test_other_nullable_helpers_remain_unsupported(self):
        self.items = decoders.parse_items(RUST.replace(
            "pub(crate) deadline: Option<u64>,",
            '#[serde(deserialize_with = "another::nullable")]\n'
            "    pub(crate) deadline: Option<u64>,"))
        self.assertIn("unsupported serde syntax", "\n".join(self.errors()))

    def test_required_nullable_non_option_remains_unsupported(self):
        self.items = decoders.parse_items(RUST.replace(
            "pub(crate) name: String,",
            '#[serde(deserialize_with = "crate::lean_vocab_test::required_nullable")]\n'
            "    pub(crate) name: String,"))
        self.assertIn("required_nullable on non-Option field", "\n".join(self.errors()))

    def test_relative_required_nullable_non_option_remains_unsupported(self):
        self.items = decoders.parse_items(RUST.replace(
            "pub(crate) name: String,",
            '#[serde(deserialize_with = "required_nullable")]\n'
            "    pub(crate) name: String,"))
        self.assertIn("required_nullable on non-Option field", "\n".join(self.errors()))

    def test_unknown_field_rejected(self):
        self.assertIn("unknown field `extra`", "\n".join(
            self.errors(lambda case: case.__setitem__("extra", 1))))

    def test_camel_case_variant_names(self):
        self.items = decoders.parse_items(RUST.replace('"snake_case"', '"camelCase"'))
        payload = copy.deepcopy(VALID)
        payload["cases"][0]["steps"][0]["operation"] = "renewLease"
        self.assertEqual(decoders.validate(self.items, payload, GROUPS), [])
        self.assertIn("unknown variant `renew_lease`", "\n".join(self.errors()))

    def test_explicit_variant_rename_overrides_rename_all(self):
        self.items = decoders.parse_items(RUST.replace(
            "    Complete,", '    #[serde(rename = "complete-now")]\n    Complete,'))
        self.assertEqual(self.errors(), [])
        self.assertEqual(self.errors(lambda case: case.__setitem__("outcome", "complete-now")), [])
        self.assertIn("unknown unit variant `complete`", "\n".join(
            self.errors(lambda case: case.__setitem__("outcome", "complete"))))

    def test_missing_required_field_rejected(self):
        self.assertIn("missing field `name`", "\n".join(
            self.errors(lambda case: case.pop("name"))))

    def test_unknown_tagged_variant_rejected(self):
        self.assertIn("unknown variant `teleport`", "\n".join(
            self.errors(lambda case: case["steps"][0].__setitem__("operation", "teleport"))))

    def test_unknown_field_in_tagged_variant_rejected(self):
        self.assertIn("unknown field `later`", "\n".join(
            self.errors(lambda case: case["steps"][0].__setitem__("later", 1))))

    def test_unknown_unit_variant_rejected(self):
        self.assertIn("unknown unit variant `retracted`", "\n".join(
            self.errors(lambda case: case.__setitem__("outcome", "retracted"))))

    def test_bool_is_not_an_integer(self):
        self.assertIn("expected u64", "\n".join(
            self.errors(lambda case: case["steps"][0].__setitem__("now", True))))

    def test_negative_unsigned_rejected(self):
        self.assertIn("expected u64", "\n".join(
            self.errors(lambda case: case["steps"][0].__setitem__("now", -1))))

    def test_fixed_width_integer_overflow_rejected(self):
        rust = RUST.replace("now: u64", "now: u8")
        items = decoders.parse_items(rust)
        payload = copy.deepcopy(VALID)
        payload["cases"][0]["steps"][0]["now"] = 256
        self.assertIn("expected u8", "\n".join(decoders.validate(items, payload, GROUPS)))

        rust = RUST.replace("now: u64", "now: i32")
        items = decoders.parse_items(rust)
        payload["cases"][0]["steps"][0]["now"] = 2**31
        self.assertIn("expected i32", "\n".join(decoders.validate(items, payload, GROUPS)))

    def test_renamed_tagged_variant_rejects_original_name(self):
        rust = RUST.replace("RenewLease { now: u64 }",
                            '#[serde(rename = "renewed")] RenewLease { now: u64 }')
        errors = decoders.validate(decoders.parse_items(rust), VALID, GROUPS)
        self.assertIn('unknown variant `renew_lease`', "\n".join(errors))
        payload = copy.deepcopy(VALID)
        payload["cases"][0]["steps"][0]["operation"] = "renewed"
        self.assertEqual(decoders.validate(decoders.parse_items(rust), payload, GROUPS), [])

    def test_unsupported_reachable_serde_features_fail_closed(self):
        for attribute in ["flatten", 'deserialize_with = "decode"']:
            rust = RUST.replace("pub(crate) notes: Vec<String>,",
                                f"#[serde({attribute})]\npub(crate) notes: Vec<String>,")
            self.assertIn("unsupported serde syntax", "\n".join(
                decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

        rust = RUST.replace("#[serde(rename_all = \"snake_case\")]\npub(crate) enum Outcome",
                            "#[serde(untagged)]\npub(crate) enum Outcome")
        self.assertIn("unsupported serde syntax", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

    def test_custom_deserialize_fails_closed_when_reachable(self):
        rust = RUST + "\nimpl<'de> Deserialize<'de> for Outcome {}\n"
        self.assertIn("custom Deserialize implementation", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

    def test_unsupported_layouts_fail_closed(self):
        rust = RUST.replace("pub(crate) notes: Vec<String>,",
                            "pub(crate) notes: Vec<String>,\npub(crate) malformed;")
        self.assertIn("unsupported field layout", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

        rust = RUST.replace("Partial,", "Partial,\nBroken = 7,")
        self.assertIn("unsupported variant layout", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

    def test_cfg_and_unselected_variant_attributes_fail_closed(self):
        rust = RUST.replace("pub(crate) struct Case<T>",
                            '#[cfg(feature = "fixture")]\npub(crate) struct Case<T>')
        self.assertIn("cfg", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

        rust = RUST.replace("Release,", "#[cfg(any())]\nRelease,")
        self.assertIn("cfg", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

    def test_missing_derive_and_struct_container_transforms_fail_closed(self):
        rust = RUST.replace("#[derive(Debug, Deserialize)]\n#[serde(deny_unknown_fields)]\npub(crate) struct Case<T>",
                            "#[derive(Debug)]\n#[serde(deny_unknown_fields)]\npub(crate) struct Case<T>")
        self.assertIn("missing derived Deserialize", "\n".join(
            decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

        for option in ['rename_all = "snake_case"', 'tag = "kind"']:
            rust = RUST.replace("#[serde(deny_unknown_fields)]\npub(crate) struct Case<T>",
                                f"#[serde(deny_unknown_fields, {option})]\npub(crate) struct Case<T>")
            self.assertIn(option.split()[0], "\n".join(
                decoders.validate(decoders.parse_items(rust), VALID, GROUPS)))

    def test_tuple_arity_and_generic_substitution(self):
        self.assertIn("2-tuple", "\n".join(
            self.errors(lambda case: case.__setitem__("owner", [1]))))
        self.assertIn("expected bool", "\n".join(
            self.errors(lambda case: case.__setitem__("payload", "yes"))))

    def test_reexported_protocol_type_is_presence_only(self):
        self.assertEqual(self.errors(lambda case: case.__setitem__("phase", "anything")), [])
        self.assertIn("got null", "\n".join(
            self.errors(lambda case: case.__setitem__("phase", None))))

    def test_missing_group_rejected(self):
        self.assertIn("group absent", "\n".join(
            decoders.validate(self.items, {}, GROUPS)))

    def test_contract_markers_are_honored(self):
        raw = "log {noise}\n" + decoders.BEGIN + '\n{"cases": []}\n' + decoders.END + "\ntrailer }"
        self.assertEqual(decoders.extract_contract(raw), {"cases": []})


if __name__ == "__main__":
    unittest.main()
