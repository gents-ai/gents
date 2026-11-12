# Upgrading from v1.0.0-rc1 (or v1.0.0-rc0)

v1.0.0-rc2 is a breaking release. It does not open, convert or migrate a home
that v1.0.0-rc1 or v1.0.0-rc0 created. Start a new home.

## What an older home does

Starting v1.0.0-rc2 on an rc1 home stops before any write, with exit code 65:

```
Error: this home was created by an older Gents version (v1.0.0-rc1 or earlier),
which this release cannot open or convert. Back the home up or delete it, then
run `gents` to set up a fresh one. To keep anything, export it first with the
release that created it.
```

Nothing in the store is changed; only the home's `data.lock` claim file is
rewritten. The desktop shows the same refusal on its older-home screen and
offers to start fresh. This was checked by creating a home with the released
v1.0.0-rc1 CLI (`gents init`) and reading it offline with the rc2 CLI
(`gents config trigger list --home <home>`).

The refusal is decided by the store itself, before any migration runs: an rc1
store has no backend protection provenance, and its `Node` collection predates
the native identity baseline. A home whose store is on another unknown schema
lineage is refused the same way.

## A home from a development build

A home created by a build of `main` after v1.0.0-rc1 that already has backend
protection and the native identity baseline is on rc2's lineage and opens in
place. On first open rc2 registers the collections it adds (`Page`,
`PluginSource`, `AgentRequestBackendWait`) and appends `create_session` and
`max_fires_per_hour` to `Trigger` through a versioned upgrade step; existing
triggers keep their rows and read both fields as unset. This is covered by the
migration suite, which opens a store holding the previous `Trigger` schema; it
was not rerun with a CLI built from `main`.

## Starting fresh

1. Stop the old runtime with the release that runs it:
   `gents service stop --home <home>` on v1.0.0-rc1, or quit the desktop and
   choose Stop Node.
2. If you need anything from the old home, export it with the release that
   created it (`gents config export --root <dir> --home <home>` on v1.0.0-rc1) before you
   replace the binary. Sessions and transcripts are not carried over.
3. Move the old home aside (`mv ~/.gents ~/.gents-rc1`) or delete it.
4. Install v1.0.0-rc2 and run `gents` in a terminal, or open the desktop. Both
   set up a fresh home with the Engineer and ask you to approve the permissions
   its starter packs request. After setup the desktop shows a "Starter packs"
   card listing the Engineer's six starter packs and the permissions each
   requests. Approve them one by one or all at once; approving restarts the
   local node for a moment while the packs install. "Not now" closes the card,
   and the same list stays in the Packs screen.
5. Re-apply exported configuration with `gents config apply --root <dir>`, and re-enroll
   clients and peers: the new home has a new node identity.

## Peers and desktops

A desktop upgrades with the node it pairs with, and both start from fresh
homes. An rc2 node does not sync with an rc0 or rc1 peer: their replicated
schemas differ, so upgrade every node of a pairing together.

The rc0-to-rc1 notes are in [upgrade-from-rc0.md](upgrade-from-rc0.md); they
describe the in-place upgrade that v1.0.0-rc1 performed and do not apply to
rc2.
