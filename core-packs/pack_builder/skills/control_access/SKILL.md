---
name: control-access
description: Load when deciding who sees or changes app data, or when sharing an app. Says what the node enforces.
---

# Control access

## Write the table first

For each collection: who reads, who writes, who must never see it. Use the person's words. Most apps fit one of these:

- **Private**: only the installer.
- **Shared by the app's agents**: only those agents read and write.
- **Per-record owner**: people see their own records.
- **Append-only log**: members add entries; nobody changes them.

## Check what the node can enforce

Before promising a restriction, read `schema help collections policy` and `p2p help scope`. A node a person joins can expose more than one app's data. If the request is "share this with my team and nothing else" and the node can't limit that, say so before building and offer what it can do. Never describe a restriction as in place until you have seen it refuse someone. Test the forbidden action from a different identity, next to an allowed one; a success alone proves half.

Next: `package-pack`.
