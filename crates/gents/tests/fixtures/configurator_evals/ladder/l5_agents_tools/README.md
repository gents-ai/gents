# configurator-l5-agents-tools

## Configuration

The Engineer makes agents it can hand work to, and the work reaches the right
agent and conversation and comes back.
The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).
No check names a tool the prompt did not ask for.

## Cases

| Case | User need | Graded by |
| --- | --- | --- |
| train-minimal-self-target | a helper to hand work to, then a follow-up in the same conversation | a target for the helper; its reply returns; the follow-up lands in the one conversation |
| val-caller-agent-and-delegate | a coordinator that hands work to a shared helper | one target for the helper, selected by the coordinator; a reply relayed helper to coordinator to Engineer |
| val-self-target-existing-agent | another session of the Engineer itself | a second Engineer session answers and its reply returns |
| val-two-targets-both-selected | two helpers | a target for each; each replies |
| val-ghost-agent-refused | an assistant that does not exist | no stand-in agent or target and a reply that says so; once the user asks for it, the new agent replies |
| val-repoint-target-in-place | a named connection moved to a different agent | one connection named helper, pointing at the new agent, which then replies |
| val-verify-then-delegate-no-dupes | a setup checked on return and then used | still one helper and one target; its reply returns |
| heldout-mixed-valid-and-ghost | one real helper and one missing assistant | the helper works; nothing stands in for the missing one |
| train-stop-one-crate-load | two conversations, one stopped, one continued | two conversations with their totals; the named one stopped, the other answers the follow-up |
| train-notify-west-dock-thread | a note to one of two existing conversations, not repeated | the note lands once in the named conversation, also after a retry request |
| train-repair-intake-acknowledgement | a note to one conversation with its acknowledgement | the note lands in the named conversation and the acknowledgement returns |
| val-stop-duplicate-invoice-batch | two conversations, one shut down, one credited | as train-stop-one-crate-load |
| val-linen-replenishment-request | a conditional request to one conversation, not repeated | as train-notify-west-dock-thread |
| heldout-night-proof-desk-notice | a note to the night desk only | as train-notify-west-dock-thread |

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval subject --out <subject>
gents utils eval run configurator-l5-agents-tools --cell engineer=<subject>:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
