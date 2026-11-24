# engineer_goals

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). As
init leaves it, it has `get_goal`, `update_goal` and `create_goal`. The runner
cannot create a goal on a trial's session, so each case starts without one and
the Engineer creates any goal itself. Goal stages settle, so the evidence
covers the goal's continuation requests.

Each goal case names its token budget the way a user would, and the budget is
tight enough that a goal which keeps going instead of finishing or blocking
stops on it. A case grades the goal's final status and budget, the files the
work leaves and, for blocked goals, the one inbox item the user asked for;
never the goal's wording except where the user asked it to change.

## Cases

| Case | Split | User need | Features exercised | Graded by |
| --- | --- | --- | --- | --- |
| no-goal-needed | train | ask whether a goal is running, then ask a one-shot question | goal status read; no goal for a plain answer | no goal exists after either turn; the replies say there is none, then west with 31 |
| pallet-totals-goal | train | a goal to total pallets across depots, with exclusions | create, work and complete a goal | complete at 300,000 tokens; totals.json holds 37 |
| queue-one-per-turn | train | a goal that prints one queued job per turn | continuation turns, completion after the last item | complete at 800,000; at least three continuations; printed.json lists the four jobs in order with their pages |
| narrow-report-objective | train | a copy goal narrowed to final reports once drafts turn up | update the objective in place | complete at 500,000 with an objective that says final; archive holds q1 and q3 unchanged and no q2 |
| tags-already-owned | train | a goal whose condition already holds | complete without edits | complete at 300,000; every tag file keeps its owner |
| points-partial-blocked | train | do what can be done, ask once, then block | partial work, one mailbox item, blocked status | blocked at 500,000; one inbox item; points.json scores only m-1 and m-2 |
| blocked-manifest | validation | block promptly on a missing input | one mailbox item, blocked status | blocked at 500,000; one inbox item; no reconcile.json |
| crate-budget-raise | validation | raise the budget only if the data says so | budget left alone when the condition fails | complete at 400,000; weights.json holds 250 |
| greenhouse-water-goal | held_out | a goal reporting active zones | create and complete | complete at 350,000; the report holds 165 and [Z1, Z3] |
| shelf-budget-raise | held_out | raise the budget when the data says so | budget raised in place | complete at 600,000; shelf-count.json holds 78 |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-goals`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
