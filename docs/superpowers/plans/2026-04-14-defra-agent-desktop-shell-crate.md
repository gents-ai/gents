# defra-agent-desktop Shell Scaffold (T2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create the new `defra-agent-desktop` crate, bundle the three required fonts, apply the dashboard theme to `egui`, and ship a runnable shell with an activity bar, identity chip, placeholder panes, and an always-visible status bar.

**Architecture:** This ticket is intentionally UI-only. The desktop app boots with local placeholder state and does not talk to `defra-node` yet. The shell establishes the permanent application structure that later tickets wire to the real client core: `main.rs` owns the eframe startup, `app.rs` owns the shell state and frame layout, `theme.rs` owns fonts and visuals, and `views/*` render per-activity placeholders that preserve the layout contract from the design spec.

**Tech Stack:** Rust 2021, `eframe`, `egui`, `tracing`.

**Reference spec:** `docs/superpowers/specs/2026-04-13-desktop-dashboard-design.md` (T2 row, aesthetic direction, and UI structure sections). Canonical visual reference: `docs/superpowers/specs/2026-04-13-desktop-dashboard-mockup.html`.

---

## Execution environment

This plan runs on `main` directly. Keep it scoped to the shell crate and shared workspace wiring only; do not start on client-core networking or replicated-store behavior here.

---

## File Structure

**New files:**

- `crates/defra-agent-desktop/Cargo.toml`
- `crates/defra-agent-desktop/src/main.rs`
- `crates/defra-agent-desktop/src/app.rs`
- `crates/defra-agent-desktop/src/theme.rs`
- `crates/defra-agent-desktop/src/state.rs`
- `crates/defra-agent-desktop/src/views/mod.rs`
- `crates/defra-agent-desktop/src/views/chat/mod.rs`
- `crates/defra-agent-desktop/src/views/operator/mod.rs`
- `crates/defra-agent-desktop/src/views/peers/mod.rs`
- `crates/defra-agent-desktop/src/views/logs/mod.rs`
- `crates/defra-agent-desktop/assets/fonts/ChakraPetch-Regular.ttf`
- `crates/defra-agent-desktop/assets/fonts/SpaceMono-Regular.ttf`
- `crates/defra-agent-desktop/assets/fonts/BigShouldersStencilDisplay-Regular.ttf`

**Modified files:**

- `Cargo.toml` (workspace root) - add desktop crate member and shared UI deps
- `Cargo.lock`

---

## Task 1: Add the desktop crate to the workspace

**Files:**

- Create: `crates/defra-agent-desktop/Cargo.toml`
- Modify: `Cargo.toml`

### Steps

- [ ] **Step 1: Register shared UI dependencies**

Add workspace dependencies for the UI stack used in later tickets:

- `eframe`
- `egui`

Reuse the existing workspace `tracing` dependency instead of adding a crate-local version.

- [ ] **Step 2: Add the desktop crate member**

Extend the workspace `members` array with `"crates/defra-agent-desktop"`.

- [ ] **Step 3: Create the desktop manifest**

Create `crates/defra-agent-desktop/Cargo.toml` with:

- package metadata inherited from the workspace
- binary crate shape only
- dependencies on `eframe.workspace = true`, `egui.workspace = true`, `tracing.workspace = true`

Do **not** add `defra-node`, `tokio`, `events`, or protocol/client-core dependencies yet; those belong to T3+.

- [ ] **Step 4: Verify workspace wiring**

Run: `cargo check -p defra-agent-desktop`

Expected: the crate resolves even before the app body is implemented.

---

## Task 2: Bundle fonts and theme tokens

**Files:**

- Create: `crates/defra-agent-desktop/assets/fonts/*`
- Create: `crates/defra-agent-desktop/src/theme.rs`

### Steps

- [ ] **Step 1: Vendor the three font assets**

Add the exact font families called out by the spec:

- `Chakra Petch`
- `Space Mono`
- `Big Shoulders Stencil Display`

Store only the regular weights needed for MVP. Keep filenames stable and ASCII.

- [ ] **Step 2: Define theme tokens**

In `theme.rs`, define:

- palette constants matching the spec colors
- semantic colors for accent, warning, danger, and info
- reusable spacing and stroke constants for the shell

Keep the palette flat. No gradients, no shadows, no CRT overlay logic.

- [ ] **Step 3: Register fonts with egui**

Implement a helper such as `install_fonts(ctx: &egui::Context)` or `font_definitions() -> egui::FontDefinitions` that:

- loads the vendored TTF bytes with `include_bytes!`
- maps body text to Chakra Petch
- maps monospaced/technical text to Space Mono
- maps headings and stenciled labels to Big Shoulders Stencil Display

- [ ] **Step 4: Apply visuals**

Implement `apply_theme(ctx: &egui::Context)` that:

- installs the fonts
- sets `egui::Visuals::dark()`
- overrides panel fills, window fills, text colors, widget strokes, selection colors, and rounded-corner radii
- keeps the look faithful to the mockup's low-shadow, 1px-border language

- [ ] **Step 5: Add narrow unit tests**

Add lightweight tests for pure helpers only, for example:

- the theme exposes the expected accent color
- the default text styles map to the intended font families

Do not attempt screenshot testing in this ticket.

---

## Task 3: Build the shell layout

**Files:**

- Create: `crates/defra-agent-desktop/src/main.rs`
- Create: `crates/defra-agent-desktop/src/app.rs`
- Create: `crates/defra-agent-desktop/src/state.rs`
- Create: `crates/defra-agent-desktop/src/views/*`

### Steps

- [ ] **Step 1: Create the app entrypoint**

`main.rs` should:

- initialize tracing for desktop logs
- construct `eframe::NativeOptions`
- set an initial window size suitable for the mockup
- call `eframe::run_native`

- [ ] **Step 2: Define shell state**

In `state.rs`, add the minimal UI-only state needed for T2:

- `Activity` enum with `Chat`, `Operator`, `Peers`, `Logs`
- `ShellState` with active activity, placeholder identity label, and placeholder status-bar values

This state should be serializable only if trivial; otherwise keep it in-memory for now.

- [ ] **Step 3: Implement `DesktopApp`**

In `app.rs`, create the root eframe app that:

- applies the theme on first frame
- lays out the left activity bar, center content area, optional right rail, and bottom status bar
- delegates content rendering to `views::*`

- [ ] **Step 4: Implement placeholder views**

Each activity view should render:

- the correct section titles from the spec
- placeholder copy describing what later tickets will populate
- the correct broad pane structure

Specific layout constraints:

- Chat: sidebar + main pane, no right rail
- Operator: sidebar + main pane + right rail
- Peers: sidebar + main pane + right rail
- Logs: full-width main pane plus right rail-compatible summary block

- [ ] **Step 5: Implement the shared chrome**

Add the permanent shell elements:

- 52px-equivalent left activity bar
- bottom identity chip with copper-dot placeholder
- monospaced status bar spanning the full width

The shell should already read like the final app even though all data is fake.

---

## Task 4: Verify the shell crate

### Steps

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Compile**

Run: `cargo check -p defra-agent-desktop`

- [ ] **Step 3: Run tests**

Run: `cargo test -p defra-agent-desktop`

- [ ] **Step 4: Smoke-check the app launches**

Run locally: `cargo run -p defra-agent-desktop`

Expected: a native window opens with the themed shell, placeholder activities, and status bar.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/defra-agent-desktop/
git commit -m "Scaffold defra-agent-desktop shell crate"
```
