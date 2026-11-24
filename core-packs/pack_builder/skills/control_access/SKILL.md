# Control access

## Write the table first

For each collection: who reads, who writes, who must never see it. Use the person's words. Most apps fit one of these:

- **Private**: only the installer.
- **Shared by the app's agents**: only those agents read and write.
- **Per-record owner**: people see their own records.
- **Append-only log**: members add entries; nobody changes them.

## Check what the node can enforce

Before promising a restriction, read `schema help collections policy` and `p2p help scope`. Tools decide which collections an agent queries or writes; a document policy decides which documents each identity reads or changes, through any tool: `config` `policy create`, its `@policy` line on a new collection, then `policy grant` per document. A document belongs to the agent that writes it. A node a person joins can expose more than one app's data: if it can't limit a request like "share this with my team and nothing else", say so before building and offer what it can do. Never call a restriction in place until you have seen it refuse a different identity next to an allowed one; a success alone proves half.

Next: load `package-or-install` to make it a pack.
