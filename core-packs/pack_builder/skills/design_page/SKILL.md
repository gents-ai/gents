---
name: design-page
description: Load before writing or changing an app page: SDK blocks, theme tokens, layout, preview check.
---

# Design a page

A page is a document on this node: create it with `config` (`help page`) and the app shows it at once and reloads it on each change. No file or pack is needed. Its module is one plain ES module with no build step, in a sandboxed frame. It imports only `@gents/ux-sdk`, `react` and `react/jsx-runtime`, writes elements with `jsx` and `jsxs`, and can't reach the app's DOM or styles. Block props: `config help page sdk`.

## Use the blocks

Use SDK blocks before any CSS. CSS is for layout only.

- **Layout**: `Page`, `Grid` (`minWidth: 360` for charts)
- **Data**: `Table` (sortable, scrolls itself), `Stat` (label, value, delta, tone), `Tabs`, `Select`
- **Charts**: `LineChart` (dates placed by time), `BarChart`, `DonutChart`
- **States**: `LoadingState`, `EmptyState`, `ErrorState` (pass `q.error`, `q.refresh`), `LastUpdated` (pass `q.updatedAt`, `q.loading`)

Smaller parts (cards, buttons, badges, rows, dialogs) are listed there too.

The page runs only the datastore tools its `data` field grants, the same surfaces an agent uses. Read data with `useQuery(ctx, surface, tool, { args, pages: 'all' })`. Rows are at `data.results`. Compute totals from rows; don't store them twice.

## Tokens

Theme tokens only, so light and dark both work: `var(--text)`, `var(--text-muted)`, `var(--heading)`, `var(--card)`, `var(--border)`, `var(--primary)`, `var(--destructive)`, `var(--success)`, `var(--warning)`, `var(--chart-1)` to `var(--chart-8)`. No hex, rgb or hsl. Never `--background` or `--foreground`: they don't exist here, and the text disappears.

## Layout

- The first screen shows the three or four numbers that matter, then the detail people act on, worst or newest first.
- Lists scroll in their own region. The page never runs off the bottom.
- Names, not internal IDs. One color means one thing. Numbers align right.
- Every section has loading, empty and error states and shows when it was last updated.
- At most one primary action per view.

## Check it

Run `config` with argv `["page","lint","<id>"]`, then `["ux","preview","page/<id>"]`. The preview renders light and dark with this node's data and reports problems. Fix every one and run it again. Check real data, no rows and a long list. If you can't render it, say so; don't describe how it looks.

Next: `automate` if it must refresh on its own, otherwise `control-access`.
