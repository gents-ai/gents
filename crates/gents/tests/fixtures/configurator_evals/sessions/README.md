# engineer_sessions

## Configuration

Session investigation eval definition over ACP-visible sessions. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `partner-agent`: a second agent on the node, `Partner`, whose sessions the
  Engineer investigates. It shares the Engineer's Context and runs on its
  inference slot. A user's node runs other agents whose work the Engineer is
  asked about.

## Cases

Each case asks about earlier work and is graded on the answer and the session
or message it cites, whichever tool found it.

| Case | User need | Graded by |
| --- | --- | --- |
| discover-old | a session from last year, among 5,000 expired requests | its session ID and that it is closed |
| count-closed | how many closed sessions carry a tag, other agents' included | the count |
| wrong-id | the right session for a misspelt ID | its session ID and title |
| filtered-discovery | which support sessions are open | the session's ID and title |
| transcript-evidence, archive-evidence | a rule told to the Engineer earlier, with the message it is in | the quoted rule and the message's document ID |
| cross-agent-visible | another agent's session the Engineer may read | its session ID, title and that it is closed |
| kestrel-newest-revision, juniper-original-dock, relay-handoff-current-window, atlas-review-close-date | one fact from the right one of several similar sessions | the fact and the session ID |
| cedar-receiving-not-recorded, birch-gate-unconfirmed | whether something was ever recorded | the stated absence and the session IDs |
| frame-reconciled-cost, frame-corrected-handoff-bay, frame-matched-weld-seal | a fact joined from two sessions | the fact and both session IDs |

`stub-recovery` is gone: it told the Engineer its earlier tool result had left
its context and graded that it re-read the stored result, a route no user asks
for.

## Usage

Install the definition with `gents config apply --root <this directory>`. Run `scripts/evals/run-ladder.sh sessions` from the repository root.
