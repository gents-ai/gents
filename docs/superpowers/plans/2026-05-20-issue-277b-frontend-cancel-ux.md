# Issue #277 — Plan B: Frontend cancel UX components + mounting

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Prereq:** Plan A landed (real `desktop_interrupt_request`, `desktop_preview_interrupt_cascade`, `cancelCause` field on snapshot rows).

**Goal:** Build the three React components specified in the operator-surfaces design (`CancelButton`, `CancelCauseBadge` + `CancelCauseDetails`, `CascadeCancelDialog`), mount them into the existing chat shell, and add Vitest + RTL tests for the behaviors that the HTML prototype demonstrated.

**Architecture:** Components live under `apps/desktop-tauri/src/components/cancelUx/`. The directory name `cancelUx/` (not `cancel-ux/`) matches the design spec's casing-collision fix called out at `docs/superpowers/specs/2026-05-20-desktop-operator-surfaces-design.md:395-401` — case-only collisions with sibling components break on Linux CI and case-sensitive macOS volumes. The cascade dialog manages its own state machine (open / loading-preview / showing / stale-redraw / submitting / closed); the interrupt button is a controlled component that delegates the cascade decision upward.

**Tech Stack:** React 18, TypeScript, Vitest, @testing-library/react, @testing-library/user-event, jest-dom matchers. Tauri commands invoked via `@tauri-apps/api/core::invoke`. Existing data flow: `projectChatShell(...)` already exposes `activeRequestId`; thread through `ChatComposer`. Existing component tests at `apps/desktop-tauri/src/lib/chat-shell.test.ts` are the pattern reference.

**Visual reference:** The approved prototype at `docs/ui-prototypes/panel-277-cancel-ux.html` is the visual source of truth. Tokens are already defined in `apps/desktop-tauri/src/styles/tokens.css` — reuse, don't duplicate.

---

## File Structure

**Create:**
- `apps/desktop-tauri/src/components/cancelUx/CancelButton.tsx`
- `apps/desktop-tauri/src/components/cancelUx/CancelButton.test.tsx`
- `apps/desktop-tauri/src/components/cancelUx/CancelCauseBadge.tsx`
- `apps/desktop-tauri/src/components/cancelUx/CancelCauseDetails.tsx`
- `apps/desktop-tauri/src/components/cancelUx/CancelCauseBadge.test.tsx`
- `apps/desktop-tauri/src/components/cancelUx/CascadeCancelDialog.tsx`
- `apps/desktop-tauri/src/components/cancelUx/CascadeCancelDialog.test.tsx`
- `apps/desktop-tauri/src/components/cancelUx/index.ts` — barrel.
- `apps/desktop-tauri/src/styles/cancel-ux.css` — scoped styles, imported from `App.css`.
- `apps/desktop-tauri/src/lib/tauri/interruptRequest.ts` — typed wrapper around `invoke("desktop_interrupt_request", ...)` and `invoke("desktop_preview_interrupt_cascade", ...)`.

**Modify:**
- `apps/desktop-tauri/src/components/chat/ChatComposer.tsx` — add props `activeRequestId: string | null`, `onInterruptClick: () => void`; render `<CancelButton>` between turn-status and Send.
- `apps/desktop-tauri/src/components/ChatWorkspace.tsx` — thread `activeRequestId` from `projectChatShell` result; provide `onInterruptClick` handler that opens the dialog or calls direct-interrupt depending on cascade-needed signal.
- `apps/desktop-tauri/src/components/Transcript.tsx:67-96` — render `<CancelCauseBadge>` next to cancelled tool calls; render details inside the existing `<details className="tool-item">` block.
- `apps/desktop-tauri/src/App.css` — `@import "./styles/cancel-ux.css";`

**Reference (don't modify):**
- `docs/ui-prototypes/panel-277-cancel-ux.html` — markup, interaction, and visual reference.
- `apps/desktop-tauri/src/lib/types/operations.ts` — `InterruptRequestResult`, `CascadeCancelPreview`, `DerivedCancelCauseView` (added in Plan A).

---

## Verification commands

```bash
( cd apps/desktop-tauri && npm test )
( cd apps/desktop-tauri && npx tsc --noEmit )
```

---

### Task 1: Typed Tauri wrapper

**Files:** `apps/desktop-tauri/src/lib/tauri/interruptRequest.ts` (create).

Wrap both Tauri commands in typed functions so components don't import `invoke` directly. Makes mocking trivial in tests (`vi.mock("./interruptRequest")`).

- [ ] Write failing test that mocks `@tauri-apps/api/core::invoke` and asserts the wrappers pass the right command names + arg shape.
- [ ] Implement:

```ts
import { invoke } from "@tauri-apps/api/core";
import type {
  CascadeCancelPreview,
  DesktopInterruptRequest,
  DesktopPreviewInterruptCascadeRequest,
  InterruptRequestResult,
} from "../types/operations";

export async function previewInterruptCascade(
  req: DesktopPreviewInterruptCascadeRequest,
): Promise<CascadeCancelPreview> {
  return invoke<CascadeCancelPreview>("desktop_preview_interrupt_cascade", { request: req });
}

export async function interruptRequest(
  req: DesktopInterruptRequest,
): Promise<InterruptRequestResult> {
  return invoke<InterruptRequestResult>("desktop_interrupt_request", { request: req });
}
```

- [ ] Run tests; commit: `tauri-bridge: typed wrappers for interrupt + cascade preview (#277)`.

---

### Task 2: `CancelButton` component

**Files:** `CancelButton.tsx` + `CancelButton.test.tsx`.

Mirrors prototype Section 1 behavior. Props:
```ts
type CancelButtonProps = {
  activeRequestId: string | null;
  turnState: string | null;
  onInterruptClick: () => void;
};
```

Visible only when `turnState` indicates an in-flight turn (match the existing turn-state check used elsewhere — read `chat-shell.test.ts` for the canonical predicate). Disabled with title `"Waiting for turn to register"` when `activeRequestId == null`.

- [ ] Write failing tests for all four states (hidden / disabled / enabled / clicking dispatches).
- [ ] Implement.
- [ ] Tests pass; commit: `cancelUx: CancelButton component with state-machine props (#277)`.

---

### Task 3: `CancelCauseBadge` + `CancelCauseDetails` components

**Files:** `CancelCauseBadge.tsx`, `CancelCauseDetails.tsx`, `CancelCauseBadge.test.tsx`.

`CancelCauseBadge` takes a `DerivedCancelCauseView` and renders a pill with the variant-specific class (`cause-userCancelled`, `cause-interrupted`, `cause-deadline`, `cause-unknown` — class names mirror the prototype). `CancelCauseDetails` renders the evidence list as a `<dl>` in a disclosure. Both components are pure / no Tauri calls.

- [ ] Write failing test: each cause variant renders the correct label + class. Evidence list renders each line as a `<dd>`.
- [ ] Implement.
- [ ] Tests pass; commit: `cancelUx: CancelCauseBadge + CancelCauseDetails (#277)`.

---

### Task 4: `CascadeCancelDialog` component

**Files:** `CascadeCancelDialog.tsx`, `CascadeCancelDialog.test.tsx`.

The richest component. State machine:
- `idle` → opens with `rootRequestId` and `agentDid` props
- `loadingPreview` → calls `previewInterruptCascade`
- `showingPreview { preview }` → renders 4 grouped lists; Cancel + Confirm buttons.
- `submitting` → confirm disabled, "Cancelling…"
- on result:
  - `accepted` → close, fire `onAccepted(interruptRequestedAt)` upward
  - `alreadyInterrupted` → close, fire `onAlreadyInterrupted()` upward
  - `stalePreview { preview }` → transition back to `showingPreview { preview, updated: true }`, re-render with "preview updated" pill; the next confirm uses the new signature

Accessibility (mandatory):
- `role="dialog"`, `aria-modal="true"`, `aria-labelledby` pointing to the heading.
- Focus trap: capture focus on open (first focusable = Cancel), restore on close (the caller passes a ref to the triggering element).
- `Escape` closes.
- Tab and Shift+Tab cycle within the dialog only.

- [ ] **TDD discipline matters here.** Write each behavior as its own failing test before implementing it:
  - opens and fetches preview
  - renders all four classification groups
  - `unknownPolicy` group has amber styling + warning copy (assert via class name + text)
  - Confirm with matching signature returns `accepted` and fires upward callback with timestamp
  - Confirm with `stalePreview` redraws in place with `aria-live` announcement "Preview updated"
  - Second confirm after `stalePreview` uses the *new* signature
  - Escape closes and restores focus to caller
  - Tab from Cancel goes to Confirm; Tab from Confirm goes back to Cancel (cycle)
  - Backdrop click closes (target === backdrop, not bubbled from content)

Mock the typed wrappers from Task 1; don't call real Tauri.

- [ ] Implement; commit `cancelUx: CascadeCancelDialog with focus trap + stalePreview redraw (#277)`.

---

### Task 5: Mount `CancelButton` in ChatComposer

**Files:** modify `apps/desktop-tauri/src/components/chat/ChatComposer.tsx` + `ChatWorkspace.tsx`.

- [ ] Add `activeRequestId` and `onInterruptClick` to `ChatComposerProps`.
- [ ] Render `<CancelButton>` in `.composer-footer`, before the Send button.
- [ ] In `ChatWorkspace.tsx:100`, source `activeRequestId` from the existing `projectChatShell(...)` result (read `lib/chat-shell.ts` to find the field — likely `chatShell.activeRequestId`).
- [ ] `onInterruptClick` handler: if the preview shows children, open `<CascadeCancelDialog>`; otherwise call `interruptRequest({ cascade: false, ... })` directly. Use a small local state `[cascadeOpen, setCascadeOpen] = useState(false)`. For determining "has children", call `previewInterruptCascade` first and check group counts — this matches the bridge contract.
- [ ] Existing tests in `chat-shell.test.ts` must continue to pass.
- [ ] Commit: `chat: mount CancelButton + cascade dialog in composer (#277)`.

---

### Task 6: Render `CancelCauseBadge` in Transcript

**Files:** modify `apps/desktop-tauri/src/components/Transcript.tsx`.

In `ToolGroups`, for each tool whose `cancelCause` is non-null, render `<CancelCauseBadge>` next to the tool name inside the `<summary>`, and `<CancelCauseDetails>` inside the `.tool-item-body`. For interrupted assistant turns (the `liveAssistant` or `assistantMessage` cases), if the matching response carries `cancelCause`, render the badge next to the role label.

- [ ] Existing transcript tests pass.
- [ ] Commit: `transcript: surface CancelCauseBadge on cancelled tool calls and interrupted turns (#277)`.

---

### Task 7: Style sheet

**Files:** create `apps/desktop-tauri/src/styles/cancel-ux.css`; modify `App.css` to import it.

Copy the relevant CSS blocks from the prototype:
- `.cause-badge`, `.cause-userCancelled`, `.cause-interrupted`, `.cause-deadline`, `.cause-unknown`
- `.dialog-backdrop`, `.dialog`, `.group`, `.group.unknown-policy`, `.preview-updated-pill`
- `.btn-warn`, `.btn-danger` if not already in `utilities.css`

Use existing tokens (`var(--source-green)`, etc.) — don't redeclare.

- [ ] Commit: `styles: cancel-ux component styles using existing tokens (#277)`.

---

### Task 8: Full-suite verification

- [ ] `( cd apps/desktop-tauri && npm test )` — all pass, including new component tests.
- [ ] `( cd apps/desktop-tauri && npx tsc --noEmit )` — typecheck clean.
- [ ] `cargo build -p defra-agent-desktop-tauri` — Tauri side still builds.
- [ ] Manual smoke: `npm run tauri dev`, open a chat, start a multi-tool agent turn, click Cancel — see the dialog, confirm, observe transcript badges populate as responses land.
- [ ] If manual smoke surfaces an unexpected runtime issue, STOP and report rather than patching blind.
- [ ] Push: `git push origin design/issue-277-cancel-ux-prototype`.

---

## Self-Review

- **Spec coverage:** Tasks 2 + 5 cover Panel 2. Tasks 3 + 6 cover Panel 4. Tasks 4 + 5 cover Panel 6. Task 7 ties styling. Task 1 is the bridge boundary.
- **Placeholder scan:** Task 4 lists behaviors rather than full test code because the test file would be ~400 lines and writing every assertion here would duplicate test-suite content the executor will see in context. Each bullet is a discrete failing test the executor writes before implementing the corresponding behavior.
- **Type consistency:** All component prop types reference `DerivedCancelCauseView`, `CascadeCancelPreview`, `InterruptRequestResult` from Plan A's TS additions — the contract is fixed.
- **Risk:** Task 5 depends on `projectChatShell` actually exposing `activeRequestId` today. If it doesn't, the executor adds a field there (it's a thin projection layer) — call out as a sub-task if needed but don't expand `projectChatShell` beyond that one field.
