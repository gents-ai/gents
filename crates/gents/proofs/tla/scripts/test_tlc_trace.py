import unittest

from tlc_trace import actor, actor_fields, constants, steps, value


LOCATION = "line 2, col 1 to line 2, col 9 of module Counter"


class TraceValues(unittest.TestCase):
    def test_sample_graph_keeps_repeated_edges_and_rejects_ambiguity(self):
        from tlc_simulation import graph_steps

        graph = ('1 [label="counter = 0"];\n2 [label="counter = 1"];\n'
                 '1 -> 2 [label="Increment"];\n2 -> 1 [label="Clear"];')
        sequence = [{"counter": i} for i in (0, 1, 0, 1)]
        steps = graph_steps(graph, sequence, {})
        self.assertEqual([step["action"] for step in steps], ["Increment", "Clear", "Increment"])
        with self.assertRaisesRegex(ValueError, "2 possible actions"):
            graph_steps(graph + '\n1 -> 2 [label="Reset"];', sequence, {})

    def test_functions_and_constants_keep_model_values_distinct_from_strings(self):
        bindings = constants('Actors = {w2, w1} Limit = 4 Title = "two  words"')
        self.assertEqual(bindings["Actors"], [{"$model": "w1"}, {"$model": "w2"}])
        self.assertEqual(bindings["Title"], "two  words")
        data = value('  ( <<w1, 1>> :> FALSE @@ "w1" :> [n |-> 2] )  ')
        self.assertEqual(data, {"$function": [["w1", {"n": 2}], [[{"$model": "w1"}, 1], False]]})

    def test_parameterized_actions_require_unambiguous_evidence(self):
        selectors = actor_fields("Advance(w) == pc[w]\n")
        old = {"pc": value('(w1 :> "open" @@ w2 :> "idle")')}
        new = {"pc": value('(w1 :> "done" @@ w2 :> "idle")')}
        self.assertEqual(actor(selectors["Advance"], old, new), {"$model": "w1"})
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            actor(selectors["Advance"], old, old)
        selectors = actor_fields(r"\* trace actors: Consume => queue.id")
        with self.assertRaisesRegex(ValueError, "changed actors"):
            actor(selectors["Consume"], {"queue": [{"id": 1}, {"id": 1}]}, {"queue": [{"id": 1}]})

    def test_single_variable_state_and_closing_edges_preserve_the_behavior(self):
        states = [("2217", '1: <Initial predicate>\ncount = 0'),
                  ("2217", f'2: <Advance {LOCATION}>\ncount = 1')]
        loop = steps(states + [("2122", f"1: Back to state: <Reset {LOCATION}>")], {})
        self.assertEqual(loop[-1], dict(action="Reset", actor=None, before={"count": 1},
                                        after={"count": 0}, backToState=1))
        stutter = steps(states + [("2218", "3: Stuttering")], {})[-1]
        self.assertEqual(stutter["before"], stutter["after"])
        self.assertEqual(stutter["backToState"], 2)

    def test_malformed_or_noncanonical_domain_is_rejected(self):
        for text in ('(w1 :> 1 @@ w1 :> 2)', '(w1 :> )', '[a |-> 1, a |-> 2]', '1 - 1'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                value(text)


if __name__ == "__main__":
    unittest.main()
