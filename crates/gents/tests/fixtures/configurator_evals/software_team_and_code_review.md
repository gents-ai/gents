Set up my agent workspace for software development. Make the changes now and verify the resulting configuration; do not merely give me instructions.

- The existing inference profiles `high`, `medium`, and `low` are already available. Create three separate working behaviors named `Builder`, `Explorer`, and `Reviewer`. Use `medium` for Builder, `low` for Explorer, and `high` for Reviewer.
- Give each behavior a concise system prompt appropriate to its role. Builder should have the write preset. Explorer and Reviewer should have the readonly preset.
- Configure each new behavior's tool root to the user's home directory: `{{USER_HOME}}`.
- Builder must be able to create project files and execute local build and test commands within that root. Persist the root explicitly; inheriting the runtime ceiling is insufficient. The acceptance check will run a small dependency-free shell test in a fresh Builder session; it requires no network access or package installation.
- Make Builder the default behavior. Keep Setup intact and available for future configuration.
- Install the bundled `code_review` graph pack. Bind its `coordinator` slot to `high`, its `worker` slot to `medium`, and its `verifier` slot to `high`.

Use previews before every write. Inspect the finished behaviors, tools, profiles, default selection, and installed pack before reporting completion.
