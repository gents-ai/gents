# @source-inc/gents-desktop-operations

Host-extensible operations rail, holds, backgrounded work, backend/MCP health,
subagent lineage, request traces, and workspace tree.

```ts
import {
  OperationsRail,
  OperationsRailProvider,
} from "@source-inc/gents-desktop-operations";

<OperationsRailProvider api={client.api} tabs={tabs}>
  <OperationsRail />
</OperationsRailProvider>;
```

```css
@import "@source-inc/gents-desktop-tokens/semantic.css";
@import "@source-inc/gents-desktop-ui/styles.css";
@import "@source-inc/gents-desktop-operations/styles.css";
/* Host semantic-token overrides come last. */
```

Declare the package layer order before imports when composing styles manually:
`tokens, primitives, components, backend-health, backgrounded-tools, holds,
mcp-health, utilities, trace, workspace`, followed by a host-owned layer.

Passing `client.api` makes every package-owned poll/read/action inside the rail
instance-bound. Standalone panels also accept an `api` prop or can be wrapped
with `OperationsApiProvider`; the process-global adapter is only a compatibility
fallback.

**Required grants:** default + operations-read + interrupt + holds + trace-read + resend-control + workspace-read.
