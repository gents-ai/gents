import copy
import unittest

from oracle import observation


class OracleTests(unittest.TestCase):
    def passed(self, state, case, stage):
        return observation(state, "did:test:trial", case, stage)["document"]["passed"]

    def test_pagination_requires_every_page_and_no_mutations(self):
        state = {
            "pages": [f"/api/invoices?page={p}" for p in (1, 2, 3)],
            "requests": [],
            "imports": [],
        }
        self.assertTrue(self.passed(state, "pagination", "paginate"))
        state["pages"].pop()
        self.assertFalse(self.passed(state, "pagination", "paginate"))
        state["pages"].append("/api/invoices?page=3")
        state["requests"].append({})
        self.assertFalse(self.passed(state, "pagination", "paginate"))

    def test_submission_requires_exact_fields_and_exactly_once(self):
        request = {
            "recipient": "Casey",
            "region": "west",
            "service": "express",
            "reference": "BR-2048",
            "confirmed": "on",
        }
        state = {"pages": [], "requests": [request], "imports": []}
        self.assertTrue(self.passed(state, "form_tabs", "submit"))
        state["requests"].append(copy.deepcopy(request))
        self.assertFalse(self.passed(state, "form_tabs", "submit"))
        state["requests"].pop()
        request["service"] = "standard"
        self.assertFalse(self.passed(state, "form_tabs", "submit"))

    def test_upload_checks_content_not_only_matching_totals(self):
        item = {
            "csv": "name,quantity\nbolts,7\nnuts,11\nwashers,4\n",
            "receipt": {"rows": 3, "quantity_total": 22},
        }
        state = {"pages": [], "requests": [], "imports": [item]}
        self.assertTrue(self.passed(state, "upload", "import"))
        item["csv"] = "name,quantity\nwrong,7\nnuts,11\nwashers,4\n"
        self.assertFalse(self.passed(state, "upload", "import"))
        state["imports"].append(copy.deepcopy(item))
        self.assertFalse(self.passed(state, "upload", "import"))

    def test_missing_state_and_unknown_stage_are_errors(self):
        with self.assertRaises(ValueError):
            observation({}, "did:test", "pagination", "paginate")
        with self.assertRaises(ValueError):
            observation(
                {"pages": [], "requests": [], "imports": []},
                "did:test",
                "pagination",
                "typo",
            )


if __name__ == "__main__":
    unittest.main()
