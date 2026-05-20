# Backgrounded Tools Panel Implementation Plan (#276 Phase 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the real Tauri implementation of the backgrounded-tools panel — including the `desktop_operations_snapshot` command body — so the rail tab matches the approved prototype at `docs/ui-prototypes/panel-276-backgrounded-tools.html`.

**Architecture:** The desktop bridge embeds `defra-agent-desktop-core` in-process (`Arc<ClientCore>` in `DesktopAppState`). The snapshot command pulls native-executor data from the in-process registry, GraphQL-queries `AgentToolCall` rows via `core.graphql_for_agent(...)`, and assembles a `DesktopOperationsSnapshot`. The React `BackgroundedToolsPanel` mounts in `OperationsRail`, fetches the snapshot through a new `fetchOperationsSnapshot` adapter, and renders the prototype's table with the same derived-state and filter logic.

**Tech Stack:** Rust (Tauri 2.x + tokio + serde_json for GraphQL responses), TypeScript + React 18 + Vitest + Testing Library, Lean 4 (CoverageLedger), BLAKE3 (signature reused from #310 — populated but emit-floor stays unused at the command boundary).

---

## Resolved Phase 2 ambiguities

These were decided in advance and apply to every task:

1. **Command name.** PROMPT.md says `list_backgrounded_tools`; the real command (registered by #310) is `desktop_operations_snapshot`. Every step uses the real name.
2. **Snapshot ownership.** Per the user's selection, this PR (#276) lands the full `desktop_operations_snapshot` body, even though the existing stub's error message names #277. The existing stub error message gets updated to remove the obsolete claim.
3. **Stuck plumbing.** Per the user's "do both" choice: add `stuck_since` and `cancel_pending_remote_ack` to `BackgroundedToolView` for the flat row model, AND continue to populate `StuckWorkDiagnosticView` (per the spec) for the banner / Stuck Work tab.
4. **Empty-state UX.** Snapshot Err is rendered as the panel's empty state with a small caption. Pseudo-bridge availability is honored.

## Known limitation called out in the PR description

The runtime does not yet *populate* `AgentToolCall.stuck_since` or `cancel_pending_remote_ack`. The schema fields exist, and this PR exposes them through the bridge, but they will be `null` for live data until upstream runtime work writes them. The component renders correctly either way — `derivedState` falls through to `running` / `background` / `deadline+` when stuck fields are null, matching the prototype's behavior for those datasets.

---

## File Structure

### Created

```
apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot.rs        (~300 LOC, Rust impl of the snapshot builder)
apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot/tests.rs  (~250 LOC, unit tests for the builder)
apps/desktop-tauri/src/components/backgroundedTools/index.ts
apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.tsx (~250 LOC, the panel component)
apps/desktop-tauri/src/components/backgroundedTools/derivedState.ts            (~80 LOC, pure functions ported from prototype JS)
apps/desktop-tauri/src/components/backgroundedTools/useOperationsSnapshot.ts   (~60 LOC, fetch hook)
apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.test.tsx (~250 LOC)
apps/desktop-tauri/src/components/backgroundedTools/derivedState.test.ts       (~120 LOC, port the prototype's derivation tests)
apps/desktop-tauri/src/styles/backgrounded-tools.css                           (port subset of prototype CSS)
```

### Modified

```
apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs              (add 2 fields to BackgroundedToolView)
apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs           (replace desktop_operations_snapshot body)
apps/desktop-tauri/src-tauri/src/bridge/snapshot/mod.rs                        (register new operations_snapshot module)
apps/desktop-tauri/src/lib/types/operations.ts                                 (mirror 2 new fields)
apps/desktop-tauri/src/lib/desktop-api.ts                                      (add fetchOperationsSnapshot adapter)
apps/desktop-tauri/src/components/ChatWorkspace.tsx                            (populate OperationsRailProvider tabs)
apps/desktop-tauri/src/App.css                                                 (one @import for backgrounded-tools.css)
crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean                (deferred → consumerCoverage entry)
crates/defra-agent/tests/support/conformance_consumers.rs                       (register the new TS test as a consumer)
```

### Untouched (intentional)

- The prototype HTML — the user explicitly approved it as design source of truth; do not revise.
- `bridge/snapshot/operations_signature.rs` — its emit-floor state machine drives a future watcher, not the synchronous command body. Adding signature plumbing here would be premature optimization.

---

## Task 1 — Add stuck fields to bridge types

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs:71-85`
- Modify: `apps/desktop-tauri/src/lib/types/operations.ts:54-68`

- [ ] **Step 1: Add `stuck_since` and `cancel_pending_remote_ack` to Rust `BackgroundedToolView`**

In `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs`, modify `BackgroundedToolView` to add the two fields just before `native_executor`:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackgroundedToolView {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub age_ms: Option<i64>,
    pub deadline_at: Option<String>,
    pub deadline_expired: bool,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub child_request_id: Option<String>,
    pub stuck_since: Option<String>,
    pub cancel_pending_remote_ack: bool,
    pub native_executor: Option<NativeExecutorStatusView>,
}
```

- [ ] **Step 2: Mirror in TypeScript `BackgroundedToolView`**

In `apps/desktop-tauri/src/lib/types/operations.ts`, modify the `BackgroundedToolView` shape to mirror the new Rust fields:

```typescript
export type BackgroundedToolView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  lifecycleState?: string | null;
  status?: string | null;
  startedAt?: string | null;
  ageMs?: number | null;
  deadlineAt?: string | null;
  deadlineExpired: boolean;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
  childRequestId?: string | null;
  stuckSince?: string | null;
  cancelPendingRemoteAck: boolean;
  nativeExecutor?: NativeExecutorStatusView | null;
};
```

- [ ] **Step 3: Confirm types still compile**

Run:
```bash
cargo check -p defra-agent-desktop-tauri 2>&1 | tail -20
```
Expected: no errors. Existing fields are unchanged; the additive fields don't break callers because the struct is `Serialize` only (not `Deserialize`).

```bash
cd apps/desktop-tauri && npx tsc --noEmit
```
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs apps/desktop-tauri/src/lib/types/operations.ts
git commit -m "types: add stuck_since + cancel_pending_remote_ack to BackgroundedToolView

Additive fields exposing the AgentToolCall stuck-state schema fields
through the desktop bridge. Required by the backgrounded-tools panel
prototype's row-level stuck rendering. cancel_pending_remote_ack
defaults to false rather than Option<bool> because the prototype's
status derivation treats null and false identically and the runtime
will write a concrete boolean.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2 — Port the prototype's derivation logic to TypeScript

**Files:**
- Create: `apps/desktop-tauri/src/components/backgroundedTools/derivedState.ts`
- Create: `apps/desktop-tauri/src/components/backgroundedTools/derivedState.test.ts`

- [ ] **Step 1: Write the failing derivation tests**

Create `apps/desktop-tauri/src/components/backgroundedTools/derivedState.test.ts`:

```typescript
import { describe, expect, it } from "vitest";
import type { BackgroundedToolView, NativeExecutorStatusView } from "../../lib/types/operations";

import {
  STUCK_DWELL_MS,
  correlateProcess,
  derivedState,
  formatAge,
} from "./derivedState";

const baseRow: BackgroundedToolView = {
  requestId: "req_a17",
  toolCallId: "tc_001",
  toolName: "grep",
  lifecycleState: "running",
  status: null,
  startedAt: new Date(Date.now() - 5_000).toISOString(),
  ageMs: 5_000,
  deadlineAt: new Date(Date.now() + 60_000).toISOString(),
  deadlineExpired: false,
  awaitMode: "background",
  cancelPolicy: "cascade",
  childRequestId: null,
  stuckSince: null,
  cancelPendingRemoteAck: false,
  nativeExecutor: null,
};

describe("derivedState", () => {
  it("returns 'running' for a healthy bg row with lifecycle_state=running", () => {
    expect(derivedState(baseRow, Date.now())).toBe("background");
  });

  it("returns 'background' when await_mode=background and lifecycle_state is unset", () => {
    expect(derivedState({ ...baseRow, lifecycleState: null }, Date.now())).toBe("background");
  });

  it("returns 'deadline+' when deadline_expired flag is set", () => {
    expect(derivedState({ ...baseRow, deadlineExpired: true }, Date.now())).toBe("deadline+");
  });

  it("returns 'cancelPending' when cancel_pending_remote_ack is true", () => {
    expect(derivedState({ ...baseRow, cancelPendingRemoteAck: true }, Date.now())).toBe("cancelPending");
  });

  it("returns 'stuck' once dwell >= STUCK_DWELL_MS", () => {
    const stuckSince = new Date(Date.now() - STUCK_DWELL_MS - 100).toISOString();
    expect(derivedState({ ...baseRow, stuckSince }, Date.now())).toBe("stuck");
  });

  it("does not return 'stuck' for stuck_since within the dwell window", () => {
    const stuckSince = new Date(Date.now() - 1_000).toISOString();
    expect(derivedState({ ...baseRow, stuckSince }, Date.now())).toBe("background");
  });

  it("prefers 'stuck' over 'cancelPending' when both apply", () => {
    const stuckSince = new Date(Date.now() - STUCK_DWELL_MS - 100).toISOString();
    expect(
      derivedState({ ...baseRow, stuckSince, cancelPendingRemoteAck: true }, Date.now()),
    ).toBe("stuck");
  });
});

describe("correlateProcess", () => {
  const ne = (overrides: Partial<NativeExecutorStatusView>): NativeExecutorStatusView => ({
    id: 901,
    pid: 41812,
    argv0: "/usr/bin/grep",
    toolName: "grep",
    startedAt: new Date().toISOString(),
    ageMs: 5_000,
    ...overrides,
  });

  it("returns pid label when a single native executor matches (tool_name, started_at ±1s)", () => {
    const row = { ...baseRow, startedAt: new Date(1_000_000).toISOString() };
    const exec = ne({ startedAt: new Date(1_000_500).toISOString() });
    expect(correlateProcess(row, [exec]).label).toBe("pid 41812");
  });

  it("returns 'native <id>' when multiple executors match within the window", () => {
    const row = { ...baseRow, startedAt: new Date(1_000_000).toISOString() };
    const e1 = ne({ id: 901, startedAt: new Date(999_900).toISOString() });
    const e2 = ne({ id: 902, startedAt: new Date(1_000_500).toISOString() });
    const result = correlateProcess(row, [e1, e2]);
    expect(result.label).toMatch(/^native /);
    expect(result.tooltip).toContain("ambiguous");
  });

  it("falls back to 'child req_<id>' when child_request_id is set and no native executor", () => {
    expect(
      correlateProcess({ ...baseRow, childRequestId: "req_b91" }, []).label,
    ).toBe("child req_b91");
  });

  it("falls back to '—' when no executor and no child request", () => {
    expect(correlateProcess(baseRow, []).label).toBe("—");
  });
});

describe("formatAge", () => {
  it("renders MM:SS for under one hour", () => {
    expect(formatAge(48_000)).toBe("00:48");
    expect(formatAge(125_000)).toBe("02:05");
  });
  it("renders HH:MM:SS for one hour or more", () => {
    expect(formatAge(3_600_000)).toBe("01:00:00");
    expect(formatAge(3_725_000)).toBe("01:02:05");
  });
  it("clamps negatives to 00:00", () => {
    expect(formatAge(-1_000)).toBe("00:00");
  });
});
```

- [ ] **Step 2: Run tests; expect ALL to fail (module does not exist)**

```bash
cd apps/desktop-tauri && npx vitest run src/components/backgroundedTools/derivedState.test.ts
```
Expected: `Cannot find module './derivedState'`.

- [ ] **Step 3: Implement `derivedState.ts`**

Create `apps/desktop-tauri/src/components/backgroundedTools/derivedState.ts`:

```typescript
import type {
  BackgroundedToolView,
  NativeExecutorStatusView,
} from "../../lib/types/operations";

export const STUCK_DWELL_MS = 5_000;
const CORRELATION_WINDOW_MS = 1_000;

export type DerivedState =
  | "running"
  | "background"
  | "stuck"
  | "cancelPending"
  | "deadline+";

export function derivedState(
  row: BackgroundedToolView,
  nowMs: number,
): DerivedState {
  const stuckDwellMs = row.stuckSince
    ? nowMs - new Date(row.stuckSince).getTime()
    : null;
  if (stuckDwellMs != null && stuckDwellMs >= STUCK_DWELL_MS) return "stuck";
  if (row.cancelPendingRemoteAck) return "cancelPending";
  if (row.deadlineExpired) return "deadline+";
  if (row.awaitMode === "background") return "background";
  return (row.lifecycleState as DerivedState | null) ?? "running";
}

export type ProcessLabel = {
  label: string;
  tooltip: string;
};

export function correlateProcess(
  row: BackgroundedToolView,
  executors: NativeExecutorStatusView[],
): ProcessLabel {
  const startedAtIso = row.startedAt;
  if (startedAtIso) {
    const start = new Date(startedAtIso).getTime();
    const candidates = executors.filter(
      (ne) =>
        ne.toolName === row.toolName &&
        Math.abs(new Date(ne.startedAt).getTime() - start) <= CORRELATION_WINDOW_MS,
    );
    if (candidates.length === 1) {
      const ne = candidates[0];
      return { label: `pid ${ne.pid}`, tooltip: `native ${ne.id} · ${ne.argv0}` };
    }
    if (candidates.length > 1) {
      const c0 = candidates[0];
      return {
        label: `native ${c0.id}`,
        tooltip: `ambiguous: ${candidates.length} candidates — ${candidates
          .map((c) => `native ${c.id}/pid ${c.pid}`)
          .join(", ")}`,
      };
    }
  }
  if (row.childRequestId) {
    return { label: `child ${row.childRequestId}`, tooltip: "subagent dispatch" };
  }
  return { label: "—", tooltip: "no native executor; in-process tool" };
}

export function formatAge(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${pad(h)}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}
```

- [ ] **Step 4: Run tests; all should pass**

```bash
cd apps/desktop-tauri && npx vitest run src/components/backgroundedTools/derivedState.test.ts
```
Expected: 12+ passing tests, no failures.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src/components/backgroundedTools/derivedState.ts apps/desktop-tauri/src/components/backgroundedTools/derivedState.test.ts
git commit -m "backgroundedTools: port derivedState/correlateProcess/formatAge from prototype

Pure functions extracted from docs/ui-prototypes/panel-276-backgrounded-tools.html
so they can be unit-tested in isolation. 5s STUCK_DWELL_MS and ±1s
process-correlation window match the prototype constants.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3 — Snapshot fetch hook + desktop-api adapter

**Files:**
- Modify: `apps/desktop-tauri/src/lib/desktop-api.ts`
- Create: `apps/desktop-tauri/src/components/backgroundedTools/useOperationsSnapshot.ts`

- [ ] **Step 1: Add `fetchOperationsSnapshot` to the adapter contract**

In `apps/desktop-tauri/src/lib/desktop-api.ts`, locate the `DesktopApiAdapter` type and the `defaultDesktopApiAdapter` object. Add a new method (alphabetical placement is fine; put it near `fetchSessionSnapshot`):

```typescript
// In DesktopApiAdapter type:
fetchOperationsSnapshot: (
  request: DesktopOperationsSnapshotRequest,
) => Promise<DesktopOperationsSnapshot>;

// In defaultDesktopApiAdapter object:
fetchOperationsSnapshot(request) {
  return invokeDesktop<DesktopOperationsSnapshot>(
    "desktop_operations_snapshot",
    { request },
  );
},
```

At the top of the file, add the type imports:

```typescript
import type {
  DesktopOperationsSnapshot,
  DesktopOperationsSnapshotRequest,
} from "./types/operations";
```

- [ ] **Step 2: Verify TS compile**

```bash
cd apps/desktop-tauri && npx tsc --noEmit
```
Expected: no errors.

- [ ] **Step 3: Implement the React hook**

Create `apps/desktop-tauri/src/components/backgroundedTools/useOperationsSnapshot.ts`:

```typescript
import { useCallback, useEffect, useRef, useState } from "react";

import { desktopApi } from "../../lib/desktop-api";
import type {
  DesktopOperationsSnapshot,
  DesktopOperationsSnapshotRequest,
} from "../../lib/types/operations";

export type OperationsSnapshotState = {
  snapshot: DesktopOperationsSnapshot | null;
  error: string | null;
  isLoading: boolean;
  refresh: () => Promise<void>;
};

const REFRESH_INTERVAL_MS = 2_000;

export function useOperationsSnapshot(
  request: DesktopOperationsSnapshotRequest,
): OperationsSnapshotState {
  const [snapshot, setSnapshot] = useState<DesktopOperationsSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const reqRef = useRef(request);
  reqRef.current = request;

  const refresh = useCallback(async () => {
    try {
      const next = await desktopApi.fetchOperationsSnapshot(reqRef.current);
      setSnapshot(next);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      if (cancelled) return;
      await refresh();
    };
    tick();
    const id = setInterval(tick, REFRESH_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [refresh]);

  return { snapshot, error, isLoading, refresh };
}
```

`desktopApi` is the exported default adapter from `desktop-api.ts`. Verify the actual export name by reading the file before this step — if it's named differently (e.g. `defaultDesktopApi`), use that.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop-tauri/src/lib/desktop-api.ts apps/desktop-tauri/src/components/backgroundedTools/useOperationsSnapshot.ts
git commit -m "backgroundedTools: add fetchOperationsSnapshot adapter + useOperationsSnapshot hook

The hook polls every 2s while mounted. Falls back to an error state
when the snapshot command returns Err so the panel can render its
empty-state placeholder cleanly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4 — Backgrounded Tools React component (TDD)

**Files:**
- Create: `apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.tsx`
- Create: `apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.test.tsx`
- Create: `apps/desktop-tauri/src/components/backgroundedTools/index.ts`
- Create: `apps/desktop-tauri/src/styles/backgrounded-tools.css`
- Modify: `apps/desktop-tauri/src/App.css` (add `@import "./styles/backgrounded-tools.css";`)

- [ ] **Step 1: Write the failing component tests**

Create `apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.test.tsx`:

```typescript
import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";

import { BackgroundedToolsPanel } from "./BackgroundedToolsPanel";
import type {
  BackgroundedToolView,
  DesktopOperationsSnapshot,
} from "../../lib/types/operations";

vi.mock("../../lib/desktop-api", () => {
  return {
    desktopApi: {
      fetchOperationsSnapshot: vi.fn(),
    },
  };
});

import { desktopApi } from "../../lib/desktop-api";

function row(overrides: Partial<BackgroundedToolView> = {}): BackgroundedToolView {
  return {
    requestId: "req_a17",
    toolCallId: `tc_${Math.random().toString(36).slice(2, 8)}`,
    toolName: "grep",
    lifecycleState: "running",
    status: null,
    startedAt: new Date(Date.now() - 4_000).toISOString(),
    ageMs: 4_000,
    deadlineAt: new Date(Date.now() + 60_000).toISOString(),
    deadlineExpired: false,
    awaitMode: "background",
    cancelPolicy: "cascade",
    childRequestId: null,
    stuckSince: null,
    cancelPendingRemoteAck: false,
    nativeExecutor: null,
    ...overrides,
  };
}

function snapshot(toolCalls: BackgroundedToolView[]): DesktopOperationsSnapshot {
  return {
    fetchedAt: new Date().toISOString(),
    agentDid: null,
    liveness: {
      expiredProcessingCount: 0,
      requests: [],
      activeToolCalls: [],
      activeNativeExecutorsAvailable: true,
      activeNativeExecutors: [],
    },
    livenessUnavailableReason: null,
    backgroundedTools: toolCalls,
    stuckDiagnostics: [],
    lineage: null,
  };
}

describe("BackgroundedToolsPanel", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("renders an empty state when the snapshot returns zero tools", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockResolvedValue(snapshot([]));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText(/no backgrounded tools/i)).toBeInTheDocument());
  });

  it("renders an error-empty-state when the snapshot command rejects", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockRejectedValue(new Error("not implemented yet"));
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText(/snapshot bridge/i)).toBeInTheDocument());
  });

  it("renders one row per backgrounded tool with derived status badges", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockResolvedValue(
      snapshot([
        row({ toolCallId: "tc_running", toolName: "grep" }),
        row({ toolCallId: "tc_deadline", toolName: "fetch_remote", deadlineExpired: true }),
      ]),
    );
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep")).toBeInTheDocument());
    expect(screen.getByText("fetch_remote")).toBeInTheDocument();
    expect(screen.getByText(/deadline\+/i)).toBeInTheDocument();
  });

  it("marks a stuck row with the row-stuck class", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockResolvedValue(
      snapshot([
        row({
          toolCallId: "tc_stuck",
          toolName: "index_repo",
          stuckSince: new Date(Date.now() - 12_000).toISOString(),
          cancelPendingRemoteAck: true,
        }),
      ]),
    );
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("index_repo")).toBeInTheDocument());
    const tr = screen.getByText("index_repo").closest("tr");
    expect(tr?.className).toContain("row-stuck");
  });

  it("hides healthy rows when 'show only stuck' toggle is on", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockResolvedValue(
      snapshot([
        row({ toolCallId: "tc_healthy", toolName: "grep" }),
        row({
          toolCallId: "tc_stuck",
          toolName: "index_repo",
          stuckSince: new Date(Date.now() - 12_000).toISOString(),
        }),
      ]),
    );
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep")).toBeInTheDocument());
    fireEvent.click(screen.getByLabelText(/show only stuck/i));
    expect(screen.queryByText("grep")).not.toBeInTheDocument();
    expect(screen.getByText("index_repo")).toBeInTheDocument();
  });

  it("filters by state chip", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockResolvedValue(
      snapshot([
        row({ toolCallId: "tc_a", toolName: "grep" }),
        row({ toolCallId: "tc_b", toolName: "fetch_remote", deadlineExpired: true }),
      ]),
    );
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /past deadline/i }));
    expect(screen.queryByText("grep")).not.toBeInTheDocument();
    expect(screen.getByText("fetch_remote")).toBeInTheDocument();
  });

  it("sorts by age descending by default and toggles to ascending on header click", async () => {
    (desktopApi.fetchOperationsSnapshot as ReturnType<typeof vi.fn>).mockResolvedValue(
      snapshot([
        row({ toolCallId: "tc_young", toolName: "grep_young", startedAt: new Date(Date.now() - 2_000).toISOString(), ageMs: 2_000 }),
        row({ toolCallId: "tc_old", toolName: "grep_old", startedAt: new Date(Date.now() - 200_000).toISOString(), ageMs: 200_000 }),
      ]),
    );
    render(<BackgroundedToolsPanel />);
    await waitFor(() => expect(screen.getByText("grep_young")).toBeInTheDocument());
    const rows = screen.getAllByRole("row").slice(1); // skip header
    expect(rows[0].textContent).toContain("grep_old");
    fireEvent.click(screen.getByRole("columnheader", { name: /age/i }));
    const rowsAsc = screen.getAllByRole("row").slice(1);
    expect(rowsAsc[0].textContent).toContain("grep_young");
  });
});
```

- [ ] **Step 2: Run tests; expect ALL to fail (component does not exist)**

```bash
cd apps/desktop-tauri && npx vitest run src/components/backgroundedTools/BackgroundedToolsPanel.test.tsx
```
Expected: `Cannot find module './BackgroundedToolsPanel'`.

- [ ] **Step 3: Port the prototype CSS subset**

Create `apps/desktop-tauri/src/styles/backgrounded-tools.css`. Copy the class rules from `docs/ui-prototypes/panel-276-backgrounded-tools.html` (every selector under `<style>` that targets `.tools-table-wrap`, `.tools`, `.row-stuck`, `.pill`, `.pill-await`, `.pill-status`, `.chip` only in the panel context, `.panel-summary`, `.panel-footer`, `.empty-state`, `.row-actions`, `.cell-tool`, `.cell-age`, `.cell-parent`, `.cell-process`, `.stuck-banner` — but NOT the prototype's dataset-switcher chips, since those don't exist in the real panel).

Top-level rules like `body { padding: 24px }` should be omitted — the panel inherits from the app shell. Wrap every rule in `.background-tools-panel` so styles are scoped:

```css
.background-tools-panel .tools-table-wrap { /* ... */ }
.background-tools-panel .pill { /* ... */ }
/* etc. */
```

This keeps the prototype's visual fidelity without polluting the global stylesheet.

- [ ] **Step 4: Add the CSS @import to App.css**

Add to `apps/desktop-tauri/src/App.css` (location near other `@import` lines):

```css
@import "./styles/backgrounded-tools.css";
```

- [ ] **Step 5: Implement the React component**

Create `apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.tsx`:

```typescript
import { useCallback, useMemo, useState } from "react";

import type {
  BackgroundedToolView,
  DesktopOperationsSnapshotRequest,
} from "../../lib/types/operations";
import {
  correlateProcess,
  derivedState,
  formatAge,
  type DerivedState,
} from "./derivedState";
import { useOperationsSnapshot } from "./useOperationsSnapshot";

type SortKey = "toolName" | "ageMs" | "requestId" | "awaitMode" | "derivedState" | "processLabel";
type SortDir = "ascending" | "descending";

export type BackgroundedToolsPanelProps = {
  rootRequestId?: string | null;
};

export function BackgroundedToolsPanel({ rootRequestId }: BackgroundedToolsPanelProps = {}) {
  const request: DesktopOperationsSnapshotRequest = useMemo(
    () => ({ rootRequestId: rootRequestId ?? null }),
    [rootRequestId],
  );
  const { snapshot, error, isLoading } = useOperationsSnapshot(request);

  const [stateFilters, setStateFilters] = useState<Set<DerivedState>>(new Set());
  const [awaitFilters, setAwaitFilters] = useState<Set<string>>(new Set());
  const [parentFilter, setParentFilter] = useState<string>("all");
  const [hideHealthy, setHideHealthy] = useState<boolean>(false);
  const [sortKey, setSortKey] = useState<SortKey>("ageMs");
  const [sortDir, setSortDir] = useState<SortDir>("descending");
  const [selectedToolCallId, setSelectedToolCallId] = useState<string | null>(null);

  const projected = useMemo(() => {
    if (!snapshot) return [];
    const now = Date.now();
    const execs = snapshot.liveness?.activeNativeExecutors ?? [];
    return snapshot.backgroundedTools.map((row) => {
      const proc = correlateProcess(row, execs);
      return {
        ...row,
        derivedState: derivedState(row, now),
        ageMs: row.ageMs ?? 0,
        processLabel: proc.label,
        processTooltip: proc.tooltip,
      };
    });
  }, [snapshot]);

  const filtered = useMemo(() => {
    const rows = projected.filter((r) => {
      if (parentFilter !== "all" && r.requestId !== parentFilter) return false;
      if (stateFilters.size > 0 && !stateFilters.has(r.derivedState)) return false;
      if (awaitFilters.size > 0 && (r.awaitMode == null || !awaitFilters.has(r.awaitMode))) return false;
      if (hideHealthy && !["stuck", "cancelPending", "deadline+"].includes(r.derivedState)) return false;
      return true;
    });
    const dir = sortDir === "ascending" ? 1 : -1;
    return [...rows].sort((a, b) => {
      const av = (a as any)[sortKey];
      const bv = (b as any)[sortKey];
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (typeof av === "number" && typeof bv === "number") return (av - bv) * dir;
      return String(av).localeCompare(String(bv)) * dir;
    });
  }, [projected, parentFilter, stateFilters, awaitFilters, hideHealthy, sortKey, sortDir]);

  const parents = useMemo(
    () => Array.from(new Set(projected.map((r) => r.requestId))),
    [projected],
  );

  const onSort = useCallback(
    (key: SortKey) => {
      if (sortKey === key) {
        setSortDir((d) => (d === "ascending" ? "descending" : "ascending"));
      } else {
        setSortKey(key);
        setSortDir("ascending");
      }
    },
    [sortKey],
  );

  const toggleStateFilter = (s: DerivedState) =>
    setStateFilters((prev) => {
      const next = new Set(prev);
      next.has(s) ? next.delete(s) : next.add(s);
      return next;
    });

  const toggleAwaitFilter = (a: string) =>
    setAwaitFilters((prev) => {
      const next = new Set(prev);
      next.has(a) ? next.delete(a) : next.add(a);
      return next;
    });

  // Error / empty state
  if (error) {
    return (
      <section className="background-tools-panel" aria-label="Background tools">
        <div className="empty-state">
          <span className="glyph" aria-hidden="true">○</span>
          Snapshot bridge unavailable: {error}
        </div>
      </section>
    );
  }

  return (
    <section className="background-tools-panel" aria-label="Background tools">
      {/* filter chips — parent, state, await, hide-healthy */}
      <div className="chip-row" role="group" aria-label="Filter by parent">
        <span className="chip-label">Parent</span>
        <button
          type="button"
          className={`chip ${parentFilter === "all" ? "is-active" : ""}`}
          aria-pressed={parentFilter === "all"}
          onClick={() => setParentFilter("all")}
        >
          All
        </button>
        {parents.map((p) => (
          <button
            key={p}
            type="button"
            className={`chip ${parentFilter === p ? "is-active" : ""}`}
            aria-pressed={parentFilter === p}
            onClick={() => setParentFilter(p)}
          >
            {p}
          </button>
        ))}
      </div>
      <div className="chip-row" role="group" aria-label="Filter by state">
        <span className="chip-label">State</span>
        {(["running", "background", "stuck", "cancelPending", "deadline+"] as DerivedState[]).map((s) => (
          <button
            key={s}
            type="button"
            className={`chip ${stateFilters.has(s) ? "is-active" : ""}`}
            aria-pressed={stateFilters.has(s)}
            onClick={() => toggleStateFilter(s)}
          >
            {s === "deadline+" ? "Past deadline" : s.charAt(0).toUpperCase() + s.slice(1)}
          </button>
        ))}
      </div>
      <div className="chip-row" role="group" aria-label="Filter by await mode">
        <span className="chip-label">Await</span>
        {["background", "bridge", "detach"].map((a) => (
          <button
            key={a}
            type="button"
            className={`chip ${awaitFilters.has(a) ? "is-active" : ""}`}
            aria-pressed={awaitFilters.has(a)}
            onClick={() => toggleAwaitFilter(a)}
          >
            {a}
          </button>
        ))}
      </div>
      <div className="chip-row">
        <span className="chip-label">Threshold</span>
        <label className="toggle">
          <input
            type="checkbox"
            checked={hideHealthy}
            onChange={(e) => setHideHealthy(e.target.checked)}
          />
          Show only stuck / cancel-pending / past deadline
        </label>
      </div>

      <div className="panel-summary">
        <div className="live-count">
          <em>{filtered.length}</em> live <span className="muted">/ 8 max</span>
        </div>
      </div>

      <div className="tools-table-wrap">
        <table className="tools" role="grid">
          <thead>
            <tr>
              {(["toolName", "ageMs", "requestId", "awaitMode", "derivedState", "processLabel"] as SortKey[]).map((key) => (
                <th
                  key={key}
                  scope="col"
                  tabIndex={0}
                  aria-sort={sortKey === key ? sortDir : "none"}
                  onClick={() => onSort(key)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      onSort(key);
                    }
                  }}
                >
                  {key === "toolName"
                    ? "Tool"
                    : key === "ageMs"
                    ? "Age"
                    : key === "requestId"
                    ? "Parent"
                    : key === "awaitMode"
                    ? "Await"
                    : key === "derivedState"
                    ? "Status"
                    : "Process"}
                </th>
              ))}
              <th scope="col" aria-label="Row actions" />
            </tr>
          </thead>
          <tbody>
            {filtered.length === 0 && !isLoading && (
              <tr>
                <td colSpan={7}>
                  <div className="empty-state">
                    <span className="glyph" aria-hidden="true">○</span>
                    No backgrounded tools.
                  </div>
                </td>
              </tr>
            )}
            {filtered.map((row) => {
              const isWarn = row.derivedState === "stuck" || row.derivedState === "cancelPending";
              const isSelected = selectedToolCallId === row.toolCallId;
              return (
                <tr
                  key={row.toolCallId}
                  tabIndex={0}
                  className={[
                    isWarn ? "row-stuck" : "",
                    isSelected ? "is-selected" : "",
                  ].filter(Boolean).join(" ")}
                  onClick={() => setSelectedToolCallId(row.toolCallId)}
                  onFocus={() => setSelectedToolCallId(row.toolCallId)}
                >
                  <td className="cell-tool">{row.toolName}</td>
                  <td className="cell-age">{formatAge(row.ageMs ?? 0)}</td>
                  <td className="cell-parent">{row.requestId}</td>
                  <td>
                    <span className="pill pill-await" data-mode={row.awaitMode ?? ""}>
                      {row.awaitMode ?? "—"}
                    </span>
                  </td>
                  <td>
                    <span className="pill pill-status" data-state={row.derivedState}>
                      {row.derivedState === "stuck" || row.derivedState === "cancelPending" ? "⚠ " : ""}
                      {row.derivedState}
                    </span>
                  </td>
                  <td
                    className={`cell-process ${row.processLabel === "—" ? "is-empty" : ""}`}
                    title={row.processTooltip}
                  >
                    {row.processLabel}
                  </td>
                  <td>
                    <div className="row-actions">
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          console.log("[backgroundedTools] open-lineage", row.toolCallId, row.requestId);
                        }}
                      >
                        Lineage
                      </button>
                      <button
                        type="button"
                        className="danger"
                        onClick={(e) => {
                          e.stopPropagation();
                          console.log("[backgroundedTools] interrupt-parent", row.requestId);
                        }}
                      >
                        Interrupt
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}
```

- [ ] **Step 6: Create the `index.ts` barrel**

Create `apps/desktop-tauri/src/components/backgroundedTools/index.ts`:

```typescript
export { BackgroundedToolsPanel } from "./BackgroundedToolsPanel";
export type { BackgroundedToolsPanelProps } from "./BackgroundedToolsPanel";
```

- [ ] **Step 7: Run tests; all should pass**

```bash
cd apps/desktop-tauri && npx vitest run src/components/backgroundedTools/
```
Expected: all 7+ test cases pass.

- [ ] **Step 8: Commit**

```bash
git add apps/desktop-tauri/src/components/backgroundedTools apps/desktop-tauri/src/styles/backgrounded-tools.css apps/desktop-tauri/src/App.css
git commit -m "backgroundedTools: implement BackgroundedToolsPanel React component

Faithful TS/React port of the approved prototype at
docs/ui-prototypes/panel-276-backgrounded-tools.html. Reads from
the desktop_operations_snapshot bridge command via the new
useOperationsSnapshot hook (2s refresh interval). Filter chips,
sortable headers, stuck-row treatment, per-row actions all preserved
from the prototype. CSS is scoped under .background-tools-panel.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5 — Mount the panel into OperationsRail

**Files:**
- Modify: `apps/desktop-tauri/src/components/ChatWorkspace.tsx`

- [ ] **Step 1: Replace the empty tabs array with a real tab descriptor**

In `apps/desktop-tauri/src/components/ChatWorkspace.tsx`, locate the `<OperationsRailProvider tabs={[]}>` (around line 84). Replace with:

```tsx
import { BackgroundedToolsPanel } from "./backgroundedTools";
// ... existing imports ...

// Where the JSX builds the rail:
const operationsTabs = useMemo(
  () => [
    {
      id: "background-tools",
      label: "Background",
      render: () => <BackgroundedToolsPanel />,
    },
  ],
  [],
);

return (
  // ... outer markup ...
  <OperationsRailProvider tabs={operationsTabs}>
    {/* existing children */}
    <OperationsRail />
  </OperationsRailProvider>
);
```

Confirm `useMemo` is imported from "react" at the top of the file. If `OperationsRailTabDescriptor` is needed for typing, import it from "./operations".

- [ ] **Step 2: Run the existing operations-rail tests to confirm no regression**

```bash
cd apps/desktop-tauri && npx vitest run tests/operations-rail.test.tsx
```
Expected: existing 3 tests still pass (they use their own harness, not ChatWorkspace).

- [ ] **Step 3: Run a typecheck on the full app**

```bash
cd apps/desktop-tauri && npx tsc --noEmit
```
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop-tauri/src/components/ChatWorkspace.tsx
git commit -m "ChatWorkspace: mount BackgroundedToolsPanel as the first OperationsRail tab

Replaces the empty tabs=[] placeholder from #310's foundation with
the real background-tools tab descriptor. Future panels (#277/#285/#286)
will push their descriptors into this same array.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6 — Rust snapshot builder (TDD)

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot.rs`
- Create: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot/tests.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/mod.rs`

This task implements the snapshot builder as a *pure function over inputs*, with the Tauri command body (Task 7) handling I/O.

- [ ] **Step 1: Add the module to snapshot/mod.rs**

In `apps/desktop-tauri/src-tauri/src/bridge/snapshot/mod.rs`, add:

```rust
pub(crate) mod operations_snapshot;
```

- [ ] **Step 2: Write the failing builder tests**

Create `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot/tests.rs`:

```rust
use super::*;
use crate::bridge::types::views::{
    ActiveToolCallView, BackgroundedToolView, NativeExecutorStatusView, RuntimeLivenessView,
};

fn liveness_with(tools: Vec<ActiveToolCallView>, execs: Vec<NativeExecutorStatusView>) -> RuntimeLivenessView {
    RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: tools,
        active_native_executors_available: true,
        active_native_executors: execs,
    }
}

#[test]
fn project_filters_to_background_await_mode_only() {
    let toolcall_rows = vec![
        ToolCallRow {
            request_id: "req_a".into(),
            tool_call_id: "tc_bg".into(),
            tool_name: "grep".into(),
            lifecycle_state: Some("running".into()),
            status: None,
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: None,
            await_mode: Some("background".into()),
            cancel_policy: Some("cascade".into()),
            child_request_id: None,
            stuck_since: None,
            cancel_pending_remote_ack: false,
        },
        ToolCallRow {
            request_id: "req_a".into(),
            tool_call_id: "tc_fg".into(),
            tool_name: "grep_fg".into(),
            lifecycle_state: Some("running".into()),
            status: None,
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: None,
            await_mode: Some("foreground".into()),
            cancel_policy: None,
            child_request_id: None,
            stuck_since: None,
            cancel_pending_remote_ack: false,
        },
    ];

    let projected = project_backgrounded_tools(&toolcall_rows, &liveness_with(Vec::new(), Vec::new()));
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].tool_call_id, "tc_bg");
}

#[test]
fn project_skips_terminal_lifecycle_state() {
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "grep".into(),
        lifecycle_state: Some("completed".into()),
        status: None,
        started_at: None,
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: None,
        cancel_pending_remote_ack: false,
    }];
    let projected = project_backgrounded_tools(&rows, &liveness_with(Vec::new(), Vec::new()));
    assert!(projected.is_empty());
}

#[test]
fn project_attaches_native_executor_when_correlated() {
    let started = "2026-05-20T12:00:00Z";
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "grep".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some(started.into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: None,
        cancel_pending_remote_ack: false,
    }];
    let execs = vec![NativeExecutorStatusView {
        id: 902,
        pid: 41812,
        argv0: "/usr/bin/grep".into(),
        tool_name: Some("grep".into()),
        started_at: started.into(),
        age_ms: 5_000,
    }];
    let liveness = liveness_with(Vec::new(), execs);

    let projected = project_backgrounded_tools(&rows, &liveness);
    assert!(projected[0].native_executor.is_some());
    assert_eq!(projected[0].native_executor.as_ref().unwrap().pid, 41812);
}

#[test]
fn stuck_diagnostic_emitted_for_each_stuck_or_cancel_pending_row() {
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "index_repo".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some("2026-05-20T12:00:00Z".into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: Some("2026-05-20T12:00:00Z".into()),
        cancel_pending_remote_ack: true,
    }];
    let diagnostics = stuck_diagnostics_from_tool_calls(&rows);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].reason, "pendingRemoteCancelAck");
}
```

- [ ] **Step 3: Run tests; expect failure (module body empty)**

```bash
cargo test -p defra-agent-desktop-tauri --lib operations_snapshot 2>&1 | tail -20
```
Expected: failures because `project_backgrounded_tools`, `stuck_diagnostics_from_tool_calls`, and `ToolCallRow` do not yet exist.

- [ ] **Step 4: Implement the builder**

Create `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot.rs`:

```rust
//! Pure projection functions over runtime liveness + AgentToolCall rows.
//! The Tauri command body lives in bridge::tauri_commands::operations and
//! is responsible for I/O; this module is pure for testability.

use crate::bridge::types::views::{
    BackgroundedToolView, NativeExecutorStatusView, RuntimeLivenessView, StuckWorkDiagnosticView,
};

/// Internal shape representing one row pulled from the `AgentToolCall`
/// collection in DefraDB. The Tauri command parses GraphQL JSON into this
/// type before passing to the projection functions.
#[derive(Debug, Clone)]
pub(crate) struct ToolCallRow {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub deadline_at: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub child_request_id: Option<String>,
    pub stuck_since: Option<String>,
    pub cancel_pending_remote_ack: bool,
}

const TERMINAL_LIFECYCLE_STATES: &[&str] = &["completed", "failed", "cancelled", "timedOut", "superseded"];
const CORRELATION_WINDOW_MS: i64 = 1_000;

pub(crate) fn project_backgrounded_tools(
    rows: &[ToolCallRow],
    liveness: &RuntimeLivenessView,
) -> Vec<BackgroundedToolView> {
    rows.iter()
        .filter(|r| r.await_mode.as_deref() == Some("background"))
        .filter(|r| !r.lifecycle_state.as_deref().is_some_and(|s| TERMINAL_LIFECYCLE_STATES.contains(&s)))
        .map(|r| {
            let age_ms = age_from_started(r.started_at.as_deref(), &liveness.active_tool_calls);
            let deadline_expired = liveness
                .active_tool_calls
                .iter()
                .find(|tc| tc.tool_call_id == r.tool_call_id)
                .map(|tc| tc.deadline_expired)
                .unwrap_or(false);
            let native_executor = correlate_native_executor(r, &liveness.active_native_executors);
            BackgroundedToolView {
                request_id: r.request_id.clone(),
                tool_call_id: r.tool_call_id.clone(),
                tool_name: r.tool_name.clone(),
                lifecycle_state: r.lifecycle_state.clone(),
                status: r.status.clone(),
                started_at: r.started_at.clone(),
                age_ms,
                deadline_at: r.deadline_at.clone(),
                deadline_expired,
                await_mode: r.await_mode.clone(),
                cancel_policy: r.cancel_policy.clone(),
                child_request_id: r.child_request_id.clone(),
                stuck_since: r.stuck_since.clone(),
                cancel_pending_remote_ack: r.cancel_pending_remote_ack,
                native_executor,
            }
        })
        .collect()
}

pub(crate) fn stuck_diagnostics_from_tool_calls(rows: &[ToolCallRow]) -> Vec<StuckWorkDiagnosticView> {
    rows.iter()
        .filter(|r| r.await_mode.as_deref() == Some("background"))
        .filter(|r| !r.lifecycle_state.as_deref().is_some_and(|s| TERMINAL_LIFECYCLE_STATES.contains(&s)))
        .filter_map(|r| {
            let reason = if r.cancel_pending_remote_ack {
                "pendingRemoteCancelAck"
            } else if r.stuck_since.is_some() {
                "stuckTool"
            } else {
                return None;
            };
            Some(StuckWorkDiagnosticView {
                request_id: r.request_id.clone(),
                session_id: None,
                severity: "warning".to_string(),
                reason: reason.to_string(),
                deadline_age_ms: None,
                last_progress_age_ms: None,
                tool_call_id: Some(r.tool_call_id.clone()),
                tool_name: Some(r.tool_name.clone()),
                stuck_since: r.stuck_since.clone(),
            })
        })
        .collect()
}

fn age_from_started(started_at: Option<&str>, live: &[crate::bridge::types::views::ActiveToolCallView]) -> Option<i64> {
    // Prefer the runtime's computed running_age_ms when the row is in the live snapshot.
    if let Some(s) = started_at {
        if let Some(tc) = live.iter().find(|tc| tc.started_at.as_deref() == Some(s)) {
            return Some(tc.running_age_ms);
        }
    }
    None
}

fn correlate_native_executor(
    row: &ToolCallRow,
    execs: &[NativeExecutorStatusView],
) -> Option<NativeExecutorStatusView> {
    let started_at = row.started_at.as_deref()?;
    let started_ms = parse_iso_ms(started_at)?;
    let mut matches = execs.iter().filter(|ne| {
        ne.tool_name.as_deref() == Some(row.tool_name.as_str())
            && (parse_iso_ms(&ne.started_at).map(|m| (m - started_ms).abs() <= CORRELATION_WINDOW_MS)).unwrap_or(false)
    });
    matches.next().cloned()
}

fn parse_iso_ms(iso: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 5: Run tests; expect all to pass**

```bash
cargo test -p defra-agent-desktop-tauri --lib operations_snapshot 2>&1 | tail -20
```
Expected: 4+ tests pass.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot.rs apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot/tests.rs apps/desktop-tauri/src-tauri/src/bridge/snapshot/mod.rs
git commit -m "snapshot: add pure operations_snapshot projection functions

project_backgrounded_tools and stuck_diagnostics_from_tool_calls are
pure functions over (ToolCallRow rows, RuntimeLivenessView). The
Tauri command body wires GraphQL/liveness I/O around them in a
later commit. Tests cover await_mode filtering, terminal lifecycle
filtering, native-executor correlation, and stuck-diagnostic
emission.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7 — Implement the Tauri command body

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs`

- [ ] **Step 1: Replace the stub body for `desktop_operations_snapshot`**

In `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs`, replace the `desktop_operations_snapshot` stub with:

```rust
use chrono::Utc;
use tauri::State;

use super::super::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    BackgroundedToolView, CascadeCancelPreview, DesktopInterruptRequest,
    DesktopListSubagentTreeRequest, DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, InterruptRequestResult, NativeExecutorStatusView,
    RuntimeLivenessView, StuckWorkDiagnosticView, SubagentTreeView,
};

#[tauri::command]
pub(crate) async fn desktop_operations_snapshot(
    state: State<'_, DesktopAppState>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, String> {
    let core = current_core(&state).ok_or_else(|| "desktop bridge not initialized".to_string())?;
    let agent_did = request.agent_did.clone();

    // 1) In-process native executor snapshot — direct call into defra-agent.
    let native_executors: Vec<NativeExecutorStatusView> =
        defra_agent::native_executor_status::active_native_executors()
            .into_iter()
            .map(|ne| NativeExecutorStatusView {
                id: ne.id as i64,
                pid: ne.pid as u32,
                argv0: ne.argv0,
                tool_name: ne.tool_name,
                started_at: ne.started_at,
                age_ms: ne.age_ms,
            })
            .collect();

    // 2) GraphQL query for live AgentToolCall rows with await_mode = "background".
    let tool_call_rows = fetch_background_tool_calls(&core, agent_did.as_deref())
        .await
        .map_err(|e| format!("failed to query AgentToolCall: {e}"))?;

    // 3) Project liveness (best-effort — empty when the snapshot is not yet
    //    populated by the runtime; future Tauri commands will fill this in).
    let liveness = RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: Vec::new(),
        active_native_executors_available: true,
        active_native_executors: native_executors,
    };

    let backgrounded_tools = project_backgrounded_tools(&tool_call_rows, &liveness);
    let stuck_diagnostics = stuck_diagnostics_from_tool_calls(&tool_call_rows);

    Ok(DesktopOperationsSnapshot {
        fetched_at: Utc::now().to_rfc3339(),
        agent_did,
        liveness: Some(liveness),
        liveness_unavailable_reason: None,
        backgrounded_tools,
        stuck_diagnostics,
        lineage: None, // owned by #285
    })
}

async fn fetch_background_tool_calls(
    core: &std::sync::Arc<defra_agent_desktop_core::client::ClientCore>,
    agent_did: Option<&str>,
) -> Result<Vec<ToolCallRow>, String> {
    let did = agent_did.unwrap_or("").to_string();
    let graphql = core.graphql_for_agent(&did).await.map_err(|e| e.to_string())?;

    // Construct a GraphQL query mirroring liveness.rs lines 19-30's row
    // shape, plus the additional schema fields (stuck_since, cancel_pending_remote_ack).
    let query = r#"
        query BackgroundToolCalls {
            AgentToolCall(filter: { await_mode: { _eq: "background" } }) {
                request_id
                tool_call_id
                tool_name
                lifecycle_state
                status
                started_at
                deadline_at
                await_mode
                cancel_policy
                child_request_id
                stuck_since
                cancel_pending_remote_ack
            }
        }
    "#;

    let response_json: serde_json::Value = graphql.query(query).await.map_err(|e| e.to_string())?;
    let rows = response_json
        .get("data")
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            request_id: row.get("request_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            tool_call_id: row.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            tool_name: row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            lifecycle_state: row.get("lifecycle_state").and_then(|v| v.as_str()).map(str::to_string),
            status: row.get("status").and_then(|v| v.as_str()).map(str::to_string),
            started_at: row.get("started_at").and_then(|v| v.as_str()).map(str::to_string),
            deadline_at: row.get("deadline_at").and_then(|v| v.as_str()).map(str::to_string),
            await_mode: row.get("await_mode").and_then(|v| v.as_str()).map(str::to_string),
            cancel_policy: row.get("cancel_policy").and_then(|v| v.as_str()).map(str::to_string),
            child_request_id: row.get("child_request_id").and_then(|v| v.as_str()).map(str::to_string),
            stuck_since: row.get("stuck_since").and_then(|v| v.as_str()).map(str::to_string),
            cancel_pending_remote_ack: row.get("cancel_pending_remote_ack").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect())
}
```

Keep the other three stubs (`desktop_list_subagent_tree`, `desktop_preview_interrupt_cascade`, `desktop_interrupt_request`) untouched; they remain panel-owned by #285/#286/#283.

Update the stub error message for `desktop_operations_snapshot` is no longer needed since we're replacing the body. But check whether the existing import of `RuntimeLivenessView`, `StuckWorkDiagnosticView`, `NativeExecutorStatusView`, `BackgroundedToolView` is already present in the types `mod`. If not, add them to the `pub(crate) use ...` line in `bridge/types/mod.rs`.

- [ ] **Step 2: Verify `chrono` is in Cargo.toml**

Run:
```bash
grep -n "chrono" apps/desktop-tauri/src-tauri/Cargo.toml
```
If absent, add `chrono = { workspace = true, features = ["serde"] }` under `[dependencies]`. The workspace already pins chrono per the root `Cargo.toml`.

- [ ] **Step 3: Verify `graphql_for_agent` exists on ClientCore**

Run:
```bash
grep -rn "graphql_for_agent\|fn graphql" crates/defra-agent-desktop-core/src/ | head -10
```
Confirm the function exists and returns something with a `.query(&str)` method. **If the actual method name or signature differs**, adapt the call accordingly — the goal is "run a GraphQL query against the agent's DefraDB". If no such method exists, fall back to `core.store()` and an equivalent in-store query (see `crates/defra-agent-desktop-core/src/client/core.rs` for the actual API; this exploration may reveal a different method like `core.graphql()` without an agent argument).

- [ ] **Step 4: Build the bridge**

```bash
cargo check -p defra-agent-desktop-tauri 2>&1 | tail -30
```
Expected: builds clean. Fix any imports or method-signature mismatches discovered in Step 3.

- [ ] **Step 5: Run Rust tests**

```bash
cargo test -p defra-agent-desktop-tauri 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs apps/desktop-tauri/src-tauri/Cargo.toml
git commit -m "operations: implement desktop_operations_snapshot body

GraphQL-queries AgentToolCall rows with await_mode = 'background',
projects through operations_snapshot::project_backgrounded_tools, and
attaches in-process active_native_executors snapshot. Stuck
diagnostics derived from stuck_since + cancel_pending_remote_ack.
Lineage stays None — owned by #285 (subagent lineage view).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 8 — Promote `background-tools.operatorUi` in CoverageLedger.lean

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/tests/support/conformance_consumers.rs`

- [ ] **Step 1: Remove the deferred entry, add a consumerCoverage entry**

In `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` (around lines 149-155), the current shape is:

```lean
{ feature := "background-tools"
, required := [Surface.agentFacing]
, deferred :=
    [ (Surface.operatorCli, "#268")
    , (Surface.operatorUi, "#276")
    ]
}
```

Change `required` to include `Surface.operatorUi` and remove the `(Surface.operatorUi, "#276")` line from `deferred`:

```lean
{ feature := "background-tools"
, required := [Surface.agentFacing, Surface.operatorUi]
, deferred :=
    [ (Surface.operatorCli, "#268")
    ]
}
```

Then add a `consumerCoverage` entry in the `caseCoverage` list (the same list PR #283 modified, around line 423 in the Explore report). Mirror that pattern:

```lean
  , tagged (consumerCoverage
      "background_tool_cases"
      "BackgroundTools"
      "defra_agent_desktop_tauri::bridge::snapshot::operations_snapshot::tests::project_filters_to_background_await_mode_only")
      "background-tools" [Surface.operatorUi]
```

`background_tool_cases` and `BackgroundTools` are placeholder identifiers — match whatever convention CoverageLedger.lean already uses for related entries (e.g. agent-facing background-tools consumer coverage may already exist; use the same case-set and module names). If unsure, search:

```bash
grep -n "background-tools\|background_tool" crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
```

- [ ] **Step 2: Register the consumer in conformance_consumers.rs**

In `crates/defra-agent/tests/support/conformance_consumers.rs`, append a new entry (TypeScript variant since the consumer test is in TS):

```rust
ConformanceConsumer::TypeScriptTest {
    id: "apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.test.tsx::BackgroundedToolsPanel renders one row per backgrounded tool with derived status badges",
    app: "desktop-tauri",
    source_path: "apps/desktop-tauri/src/components/backgroundedTools/BackgroundedToolsPanel.test.tsx",
    suite: "BackgroundedToolsPanel",
    test: "renders one row per backgrounded tool with derived status badges",
},
```

AND a Rust variant for the projection unit test:

```rust
ConformanceConsumer::RustTest {
    id: "defra_agent_desktop_tauri::bridge::snapshot::operations_snapshot::tests::project_filters_to_background_await_mode_only",
    package: "defra-agent-desktop-tauri",
    source_path: "apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_snapshot/tests.rs",
    module_path: "defra_agent_desktop_tauri::bridge::snapshot::operations_snapshot::tests",
    function: "project_filters_to_background_await_mode_only",
},
```

- [ ] **Step 3: Build the proofs**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -20
```
Expected: builds clean.

- [ ] **Step 4: Run the drift test**

```bash
cargo test -p defra-agent --test state_machine_conformance lean_feature_matrix 2>&1 | tail -20
```
Expected: pass. The feature now declares `operatorUi` as required, and a ledger row tags `(background-tools, operatorUi)`.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean crates/defra-agent/tests/support/conformance_consumers.rs
git commit -m "proofs: promote background-tools.operatorUi to consumerCoverage

Moves Surface.operatorUi from deferred to required for the
background-tools feature, and registers the new BackgroundedToolsPanel
component test + Rust projection test as the consumers. Drift test
covers the binding.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9 — Verification pass

- [ ] **Step 1: Lean build**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -10
```
Expected: zero errors. No `sorry`s introduced.

- [ ] **Step 2: Rust check + tests**

```bash
cargo check -p defra-agent 2>&1 | tail -5
cargo check -p defra-agent-desktop-tauri 2>&1 | tail -5
cargo test -p defra-agent 2>&1 | tail -10
cargo test -p defra-agent-desktop-tauri 2>&1 | tail -10
```
Expected: all green.

- [ ] **Step 3: Frontend vitest**

```bash
cd apps/desktop-tauri && npm test 2>&1 | tail -20
```
Expected: all suites pass. Add `BackgroundedToolsPanel` to the suite-count.

- [ ] **Step 4: Manual UI smoke test**

```bash
cd apps/desktop-tauri && npm run tauri dev
```

In the running app:
1. Open a chat with an active agent.
2. Confirm the OperationsRail is visible on the right.
3. Confirm a "Background" tab is present.
4. Click the tab; confirm the panel renders.
   - If no backgrounded tools are running, you should see the empty state.
   - If the snapshot bridge errors (e.g., agent not yet started), confirm the error empty-state appears with the error caption.
5. Stop the dev server when done.

- [ ] **Step 5: Cycle through the prototype's mock datasets to confirm visual parity**

Open `docs/ui-prototypes/panel-276-backgrounded-tools.html` in a browser and click through the five mock datasets. Compare each visually to what the real React component renders given equivalent inputs. The colors, fonts, spacing, and pill states should match. Any drift is a bug in the port — fix it.

---

## Task 10 — Retitle PR and update description

- [ ] **Step 1: Retitle**

```bash
gh pr edit 327 --title "Backgrounded tools panel: prototype + impl (#276)"
```

- [ ] **Step 2: Append the impl summary to the PR description**

```bash
gh pr edit 327 --body-file - <<'EOF'
## Summary

- Phase 1 (already in this PR): standalone HTML prototype at `docs/ui-prototypes/panel-276-backgrounded-tools.html`.
- Phase 2 (this update): real Tauri implementation — `BackgroundedToolsPanel` React component + `desktop_operations_snapshot` command body + OperationsRail mount + ledger promotion.

## Phase 2 changes

| Surface | What changed |
|---|---|
| Rust | `desktop_operations_snapshot` body (GraphQL query + projection); pure `project_backgrounded_tools` builder with unit tests; additive `stuck_since` + `cancel_pending_remote_ack` fields on `BackgroundedToolView`. |
| TypeScript | `BackgroundedToolsPanel` React component, `useOperationsSnapshot` hook, `fetchOperationsSnapshot` adapter, scoped CSS port. |
| OperationsRail | First real tab mounted in `ChatWorkspace.tsx`. |
| Lean | `background-tools.operatorUi` deferred → required + consumerCoverage. |

## Known limitations

- `AgentToolCall.stuck_since` / `cancel_pending_remote_ack` schema fields are exposed through the bridge but **not yet populated by the runtime**. Stuck rows will only appear once upstream runtime work writes these fields. The panel renders correctly for live data either way.

## Conflict surface

Other panels (#274 / #277 / #285 / #286 / #288) replace stubs in `operations.rs` and add tabs to `ChatWorkspace.tsx`. Expect rebase if any of them land first.

## Test plan

- [ ] `cd crates/defra-agent/proofs && lake build` — no errors
- [ ] `cargo check -p defra-agent` — clean
- [ ] `cargo test -p defra-agent` — green
- [ ] `cargo test -p defra-agent-desktop-tauri` — green
- [ ] `cd apps/desktop-tauri && npm test` — all suites pass
- [ ] Manual: tauri dev → Background tab visible → mock-data parity vs. prototype

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
```

- [ ] **Step 3: Push the new commits**

```bash
git push origin design/issue-276-backgrounded-tools-prototype
```
Expected: push succeeds; PR #327 picks up the new commits.

---

## Self-Review Notes

- **Placeholder scan:** No `TBD` / `TODO` strings. One step (Task 7 Step 3) explicitly says "adapt if signature differs" because the `graphql_for_agent` API is approximate — but the step gives concrete fallback guidance (use `core.store()` / read core.rs) rather than punting.
- **Spec coverage:** Phase 2 PROMPT.md asks for (1) React component ✓ Task 4, (2) Tauri command impl ✓ Tasks 6+7, (3) Mount into OperationsRail ✓ Task 5, (4) Ledger promotion ✓ Task 8, (5) Tests ✓ Tasks 4 + 6.
- **Type consistency:** `BackgroundedToolView` field shape is consistent Rust ↔ TS (Task 1). `STUCK_DWELL_MS = 5_000` and `CORRELATION_WINDOW_MS = 1_000` are consistent between TS (Task 2) and Rust (Task 6).
- **Drift risk:** The `graphql_for_agent` call signature is the riskiest assumption. Step 3 of Task 7 instructs verification before relying on it.
