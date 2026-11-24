# configurator-l2-agent

## Configuration

The Engineer creates an agent with its own instructions and the tool access the
user described, and the agent then does a job that needs exactly that access.
The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).
No check names a tool the prompt did not ask for.

## Subject fixtures

- `returns-desk-templates`: two instruction templates, `desk-live` and
  `desk-approved`, both labeled Returns desk. A user's node holds templates
  written earlier; the case asks the Engineer to repair one from the other.

## Cases

| Case | User need | Graded by |
| --- | --- | --- |
| scout-grants-two-file-tools | a file-finding assistant, set up once and finished on return | read-only files, no commands, no self-configuration; Scout finds the file that mentions an invoice |
| analyst-grants-file-tools | an assistant that summarizes research notes with sources | the same grants; Analyst reports a figure and its source from the notes, which stay unchanged |
| archivist-full-files-no-bash | an assistant that searches the archive | the same grants; Archivist names the file and the year |
| porter-fix-in-place-disable-bash | a repository inspector whose command access is later removed | read-only commands first, none after the edit; Porter then answers from a file |
| clerk-automation-seed-fire | an assistant that acknowledges restock notes as they arrive | read-only files and nothing more; a seeded note fires, Clerk's request completes and its outcome is recorded |
| cursor-no-host-tools | a planning assistant with no tools | every tool family off; Cursor returns a three-step plan |
| ledger-prompt-edit-in-place | a records assistant that must cite file paths | the instruction added in place, grants kept; Ledger answers with the path |
| restore-desk-instructions | live instructions replaced by the approved text | both templates hold the approved text with labels kept; an assistant on the live template answers from it |

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval subject --out <subject>
gents utils eval run configurator-l2-agent --cell engineer=<subject>:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
