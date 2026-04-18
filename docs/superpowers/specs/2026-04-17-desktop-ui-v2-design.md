# Desktop UI V2 Design

Replaces the current desktop shell and screen composition with a simpler,
consistent layout model built around the actual control-plane hierarchy:

`deployment / agent / behavior / conversation`

The existing data plumbing, projections, replication, and mutation paths are
good enough to keep. The UI layer is not. V2 treats the current desktop UI as
an exploratory prototype and redesigns the shell from first principles.

## Problem

The current desktop UI has strong backend plumbing and weak presentation
structure. It feels unstable because the shell does not express one clear
layout grammar.

Symptoms:

- multiple navigation systems appear at once
- panes feel off-center and underfilled
- fixed-height and fixed-width boxes create dead space
- chat state appears far from the chat input where it is needed
- peer setup and operator configuration feel like separate tools awkwardly
  embedded in one app
- the current abstractions leak directly into the UI

The issue is not primarily typography, color, or individual spacing constants.
It is structural: the UI does not have a clean ownership model for navigation,
content, detail, and system state.

## Design goals

1. Make chat the primary surface.
2. Organize chat around the real hierarchy:
   deployments contain agents, agents contain behaviors, behaviors contain
   conversations.
3. Make deployment and peer configuration understandable without requiring
   users to think in terms of internal implementation modules.
4. Replace ad hoc layout code with a small number of reusable pane
   abstractions.
5. Eliminate most hard-coded box sizes in favor of responsive pane sizing.
6. Preserve the existing runtime/client plumbing and rewrite the UI shell on
   top of it.

## Non-goals

- redesign the replication model
- redesign the document schema
- add major new operator capabilities
- introduce a web UI
- optimize for mobile or touch

## Information architecture

V2 organizes the app around one primary mode and one focused management
workspace.

| Surface | Purpose | Primary objects |
|---|---|---|
| Chat | Talk to one behavior inside one deployment | deployment, behavior, conversation |
| Manage deployment | Configure the selected deployment and its replicated agent documents | peer, deployment, behavior/backend/tool/profile/task |

The key change from the earlier V2 sketch is that Chat remains the home
surface. Deployment management is entered from Chat through a local affordance
attached to the selected deployment, not by forcing the user to live inside a
second permanent navigation system. Desktop-local logs and diagnostics are not
their own primary surface in V2.

## Object model

The object hierarchy surfaced in the UI is:

```text
Deployment
  Agent
    Behavior
      Conversation
```

Meanings:

- `Deployment`: one saved/connected remote runtime or local runtime
- `Agent`: one agent principal replicated from that deployment
- `Behavior`: one behavior document within that agent
- `Conversation`: one chat session scoped to that agent and behavior

Configuration objects such as backends, tool selections, inference profiles,
and scheduled tasks belong to an agent and are edited within the management
workspace, not surfaced as separate app-level silos.

## Shell model

V2 has one shell model and all screens must fit within it.

```text
+--------------------------------------------------------------+
| Primary pane                                      | Inspector |
+--------------------------------------------------------------+
| Status bar                                                   |
+--------------------------------------------------------------+
```

### Pane roles

- `Primary pane`
  - primary content and interactions
  - may internally host local subsections, such as the chat selector column
  - should remain readable without the inspector

- `Inspector`
  - optional detail, editor, diagnostics, or secondary state
  - appears only when the current mode needs it
  - must reserve real layout space, never float above content

- `Status bar`
  - always visible
  - desktop-local operational telemetry only
  - should stay narrow and low-noise

## Layout rules

V2 replaces screen-specific exact sizing with clamped responsive panes.

### Width rules

- Selector column inside Chat primary pane: `250..320`
- Inspector: `320..460`
- Primary pane: fills remaining width

Widths should be computed from available window width and clamped into these
ranges. The context pane and inspector do not both need to appear in every
mode.

### Height rules

- No fixed full-width bottom slabs.
- Input areas size to content with a min/max range.
- Empty-state content should center within the main pane but respect a max
  content width.
- Lists and transcripts should expand to fill remaining height.

### Padding rules

- one outer page padding value
- one pane gap value
- one card padding value
- one list row height family

Do not encode per-screen spacing personalities unless the screen has a strong
reason to differ.

### Box rules

- cards, list rows, editors, transcripts, and inspectors use the same framing
  language
- fixed widths for inline controls are acceptable only for small buttons and
  badges
- fixed heights for multi-line content containers should be avoided

## Abstractions

The current UI likely has the wrong abstractions: shell and screen layouts are
mixed together. V2 should use a smaller, stricter set.

### Core layout abstractions

- `AppShell`
- `ContextPane`
- `ContentPane`
- `InspectorPane`
- `StatusBar`

### Shared content abstractions

- `SectionHeader`
- `ListRow`
- `SelectionTree`
- `Card`
- `EditorSurface`
- `TranscriptSurface`
- `ComposerSurface`
- `DiagnosticStrip`

### Explicit anti-abstractions

Do not create abstractions that mix shell ownership with activity-specific
content. Examples to avoid:

- global shell that always renders chat-specific navigation
- floating overlay rails for core content
- app-level rails whose only job is switching between two screens
- screen-local sidebars nested inside a global sidebar model

## Surface designs

## Chat

Chat is the primary workflow and the most important V2 screen.

### Purpose

Let the user quickly:

- select a deployment
- select an agent
- select a behavior
- switch conversations
- send, retry, and inspect turns

### Layout

- Primary pane:
  - left selector column inside the pane
  - transcript and composer surface
- Inspector:
  - hidden by default
  - opens for exports, tool details, reasoning detail, or transcript metadata

### Selector column hierarchy

```text
Deployments
  Local Agent
  window-2
  window-3
```

Then, for the selected deployment:

```text
Behaviors
  Default
  Coding
  Support

Conversations
  Today
  Yesterday
  Earlier
```

Behavior selection should feel like changing the current silo. Conversations
belong to the selected behavior, not to some global conversation bucket.

### Main pane behavior

Header:

- breadcrumb: `deployment / behavior`
- conversation title
- conversation title should be editable
- deployment management affordance sits on the selected deployment row in the
  selector column, not in global chrome and not as a distant page switcher
- secondary actions: export, conditional retry when actually available

Transcript:

- fills the main pane vertically
- no unnecessary dead band above or below
- tool calls and reasoning should remain near the transcript, not far away in
  another unrelated screen region
- each message bubble should support copy
- busy state should appear directly beneath the last visible chat bubble, not
  in a remote toolbar or detached footer

Composer:

- anchored at bottom of the main pane
- content-driven height with min/max clamp
- send state lives near the input
- turn state appears inline with transcript flow when a turn is active
- examples:
  - `turn streaming...`
  - `turn waiting for observation...`
  - `turn failed`

### Chat design rules

- the active behavior must always be obvious
- the active conversation must always be obvious
- the current turn state must always be visible inside the chat flow
- if a request retries, the transcript should still read as one user turn plus
  retry/failure state, not multiple duplicated user messages
- the new conversation action belongs in the selector column, not as a distant
  top-right page button
- behaviors should expose compact tool capability hints

Suggested compact behavior hint model:

- `F` for file tools
- `B` for bash
- `M` for meta tools
- optional overflow tooltip or inspector detail for the full tool selection

## Manage deployment

The deployment management surface combines the old Peers and Operator concepts.

### Purpose

Let the user:

- add and remove deployments
- inspect connection and replication health
- configure a selected agent
- edit behaviors, backends, tools, profiles, and scheduled tasks

### Layout

- Primary pane:
  - full-screen editor workspace
  - section tabs across the top
  - selection picklists where needed, especially for behaviors
- Inspector:
  - optional diagnostics for the selected deployment
  - should default to health and debugging information rather than duplicating
    the main editor

### Configuration sections

Within a selected agent:

- Runtime
- Behaviors
- Backends
- Tool selections
- Inference profiles
- Scheduled tasks
- Request timeline
- Recent failures

These are agent-scoped sections, not app-scoped activities.

### Design rules

- deployment selection should clearly scope the rest of the screen
- section navigation should appear once, in one place, ideally as top tabs
- editors should be full-screen when editing a selected entity, not crushed
  into a narrow side pane
- `repair replication` should not be a mysterious global call-to-action sitting
  beside document editing. Replication and P2P health should instead be visible
  as explicit diagnostics for the selected deployment.

Recommended inspector content:

- P2P health
- replication status
- peer address and peer id
- last replication error
- recent reconnects / repair attempts

## First launch and onboarding

First launch must be simpler than today.

### Flow

1. Show local desktop readiness.
2. Ask the user to add or discover a deployment.
3. Once a deployment is connected, route the user into Chat with that
   deployment selected.

### Onboarding rules

- no hidden toggled forms in cramped sidebars
- setup forms should appear in the primary pane as first-class content
- after the first deployment is added, the app should transition naturally
  into Chat

## Interaction rules

### Selection rules

- selecting a deployment scopes the behaviors
- selecting a behavior scopes the conversations
- changing behavior does not silently preserve stale conversation state from a
  different behavior

### Retry and failure rules

- retries belong to one visible turn
- retry UI should appear only when available or strongly relevant
- request failure and retry status should surface in the chat pane, near the
  turn that failed
- system state should not echo the same user prompt multiple times in the main
  transcript

### Keyboard rules

- command palette remains valuable, but it is secondary
- primary workflows must work cleanly without it

## Visual direction

Keep the existing retro-machine register, but use it more sparingly.

Principles:

- less decorative chrome around layout containers
- stronger alignment and pane edges
- fewer competing borders and nested groups
- more breathing room for primary content
- make chat feel like a tool the user can live in for hours

The aesthetic should support the hierarchy, not compete with it.

## Implementation strategy

V2 should be built as a shell rewrite on top of the existing plumbing.

Keep:

- client core
- replicated store
- projections
- actions/controllers
- protocol types
- mutation paths

Rewrite:

- app shell
- activity routing
- pane layout
- screen composition
- most UI primitives

## Recommended implementation order

1. Chat V2 shell and selector column
2. Deployment management workspace
3. Desktop diagnostics surface
4. onboarding and first-launch flow

Chat goes first because it is the most important workflow and the clearest test
of whether the new abstractions are working.

## Acceptance criteria

V2 is successful when:

- chat clearly communicates `deployment / behavior / conversation`
- there is no second competing app-level sidebar
- the transcript and composer share space naturally with no large dead zones
- the current turn state is visible inside the chat flow
- deployment configuration lives in a full-screen operational workspace
- peer setup no longer relies on awkward toggled sidebar forms
- window resizing preserves compositional balance across wide and narrow
  desktop sizes

## Open questions

1. Should management open as a full-screen replacement surface, or as a modal
   workspace layered above Chat while preserving chat context?
2. Should behavior switching remain in the left selector column, or should the
   current behavior become a segmented header control with the other behaviors
   available in a popover?

## Recommendation

Proceed with a V2 shell rewrite centered on Chat.

The earlier V2 sketch was still too sidebar-heavy. This revised V2 direction is
better aligned with the actual user workflow: one main chat surface and one
targeted management workspace, with desktop-local diagnostics folded into
status, inspectors, or contextual debugging surfaces instead of a separate
screen.
