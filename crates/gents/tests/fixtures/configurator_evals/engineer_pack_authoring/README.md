# engineer_pack_authoring

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). It holds the pack and pack-authority grants.
Cases put draft packs in the trial workspace as fixture files and grade what
the user ends up with: the PackInstallation rows, the installed documents and
schemas, the pack files they asked to keep, and, where something was
installed, that it works: a record added to the collection the pack ships, a
skill the Engineer can now use, a task the pack ships run once. No check names
a tool or asks how a pack was checked or built. No case compiles plugin source
or reaches the registry.

## Cases

| Case | Split | User need | Features exercised | Graded by |
| --- | --- | --- | --- | --- |
| stockroom-register-pack | train | package a collection as a pack, install it, use it | pack init and install, schema publish, a write grant on the new collection | installed 1.0.0 with the StockroomBin schema; then a StockroomBin A-01 record with 40 hex bolts |
| escalation-surface-export | train | carry live configuration to another node as a pack file | pack export and build, no install | manifest name, namespace and version, the surface in pack_config.json, the .pack file; nothing installed |
| handbook-manifest-repair | train | fix a pack that fails its check, install it and use its skills | pack check findings, install, attaching skills to the Engineer | installed with both Skill documents and their text; other files unchanged; the Engineer's context lists both skills and its reply answers from them |
| tide-notes-version-bump | train | add a skill and roll the installed pack forward | release a new version, update in place | 1.0.0 installed, then 1.1.0 with the slack-water skill; the existing skill file unchanged |
| courier-zones-check-clean | train | make a pack pass its check without touching the schema or version | pack check findings, install | installed 0.3.0 with the CourierZone schema; manifest and README fixed; schema file unchanged |
| pallet-pack-remove | train | install a pack, then remove it but keep its collection | install, remove | installed with its skill and schema; then no installation and no skill, the collection kept |
| calibration-copies-audit | validation | tell two copies of a release apart without installing | comparing packs | the reply says they differ and names reference.txt; nothing installed or changed |
| berth-watch-preview | validation | see what an install would add, then install and try it | install preview, inference slot binding, a manual task run | nothing installed or changed while previewing, and the reply names the agent, task and slot; then 0.2.0 installed and the overstay task run to completion |
| parts-desk-app-pack | held_out | package an app with a datastore surface and use it | schemas, a DatastoreToolSurface, selecting it in Tools | installed 2.0.0 with both schemas and both surface tools; then a PartRequest for B-12, 3, Dana |
| kiln-runbook-missing-skill | held_out | register a skill the pack left out | pack_config repair, install | both skills installed with the firing-curve text; skill files unchanged |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-pack-authoring`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
