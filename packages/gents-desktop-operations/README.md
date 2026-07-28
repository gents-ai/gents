# @source-inc/gents-desktop-operations

Host-extensible operations rail, holds, backgrounded work, backend/MCP health,
subagent lineage, request traces, and workspace tree.

```ts
import {
  OperationsRail,
  OperationsRailProvider,
} from "@source-inc/gents-desktop-operations";
import "@source-inc/gents-desktop-operations/styles.css";
```

**Required grants:** default + operations-read + interrupt + holds + trace-read + resend-control + workspace-read.
