// Board: the shape of hermes-agent's kanban plugin on Gents, on documents.
// The plugin's own page (PLUGIN_PAGE_AREA stands in for ROUTES_AREA), a nav
// row that opens it (NAV_AREA for SIDEBAR_NAV_AREA), a ::board{id} card the
// agent can emit (TRANSCRIPT_DIRECTIVE_AREA, as in Hermes), and the `board`
// surface of ../../../board_docs as the data half: ctx.data where Hermes
// uses ctx.rest(). The page observes documents and writes documents; it
// owns no processing. Plain ESM, lint-clean, no build.
import {
  Button,
  Group,
  Input,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  Row,
  TRANSCRIPT_DIRECTIVE_AREA,
  toast,
  useQuery,
  useWrite,
} from '@gents/ux-sdk'
import { useMemo, useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

/* the surface and tools this plugin's manifest declares; the desktop
   refuses any other pair, so these names are the whole data contract */
const SURFACE = 'board'
const LANES = ['triage', 'ready', 'running', 'done']
/* a move here takes a task off the board; the document stays */
const ARCHIVED = 'archived'
/* what a ::board{id} card may name: the attrs are model output */
const TASK_ID = /^[a-z0-9][a-z0-9_-]{0,39}$/

/* the task a ::board{id} card points at: set by the card's click, read by
   the page it opens, cleared once drawn */
let highlightTask = null

const now = () => new Date().toISOString()
const mintId = () => `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`

/* the board as the documents say it is: every task, its latest move
   deciding the lane; archived ones left out */
function fold(tasks, moves) {
  const latest = new Map()
  for (const m of moves) {
    const prev = latest.get(m.task_id)
    if (!prev || m.moved_at > prev.moved_at) latest.set(m.task_id, m)
  }
  return tasks
    .map((t) => ({ ...t, lane: latest.get(t.task_id)?.lane ?? t.lane }))
    .filter((t) => t.lane !== ARCHIVED)
    .sort((a, b) => (a.created_at < b.created_at ? -1 : a.created_at > b.created_at ? 1 : 0))
}

/* two live queries and two create tools; `useQuery` re-runs on the shell's
   client-updated (an agent wrote through the same surface) and on a
   bounded interval while the page is mounted */
function useBoard(ctx) {
  const tasks = useQuery(ctx, SURFACE, 'list_tasks', { args: { limit: 1000 } })
  const moves = useQuery(ctx, SURFACE, 'list_moves', { args: { limit: 1000 } })
  const addTask = useWrite(ctx, SURFACE, 'add_task')
  const moveTask = useWrite(ctx, SURFACE, 'move_task')
  const rows = useMemo(
    () => fold(tasks.data?.results ?? [], moves.data?.results ?? []),
    [tasks.data, moves.data],
  )
  const refresh = () => {
    tasks.refresh()
    moves.refresh()
  }
  return {
    rows,
    loading: tasks.loading || moves.loading,
    error: tasks.error ?? moves.error ?? addTask.error ?? moveTask.error,
    pending: addTask.pending || moveTask.pending,
    refresh,
    add: async (title) => {
      await addTask.write({ task_id: mintId(), title, lane: 'triage', created_at: now() })
      refresh()
    },
    move: async (task_id, lane) => {
      await moveTask.write({ task_id, lane, moved_at: now() })
      refresh()
    },
  }
}

function Lane({ lane, tasks, highlight, onMove }) {
  return jsxs('div', {
    className: 'gents-board-lane',
    children: [
      jsxs('h4', { className: 'gents-board-lane-title', children: [lane, ' ', jsx('span', { children: tasks.length })] }),
      ...tasks.map((t) =>
        jsxs('div', {
          className: t.task_id === highlight ? 'gents-board-card gents-board-card-highlight' : 'gents-board-card',
          'data-task': t.task_id,
          children: [
            jsxs('span', { className: 'gents-board-card-title', children: [jsx('code', { children: `#${t.task_id}` }), ' ', t.title] }),
            jsxs('div', {
              className: 'gents-board-card-actions',
              children: [
                ...LANES.filter((l) => l !== lane).map((l) =>
                  jsx(Button, { size: 'sm', variant: 'ghost', onClick: () => onMove(t.task_id, l), children: `→ ${l}` }, l),
                ),
                jsx(Button, { size: 'sm', variant: 'ghost', title: 'Archive', onClick: () => onMove(t.task_id, ARCHIVED), children: '✕' }),
              ],
            }),
          ],
        }, t.task_id),
      ),
    ],
  })
}

function BoardPage({ ctx }) {
  const board = useBoard(ctx)
  /* the task a card deep-linked to, drawn highlighted when the board has it */
  const [highlight] = useState(() => {
    const id = highlightTask
    highlightTask = null
    return id
  })
  const [title, setTitle] = useState('')
  const add = async () => {
    if (!title.trim()) return
    await board.add(title.trim())
    setTitle('')
  }
  return jsxs('div', {
    children: [
      jsxs(Group, {
        title: 'Board',
        action: jsx(Button, { size: 'sm', variant: 'ghost', onClick: board.refresh, children: board.loading ? 'Loading…' : 'Refresh' }),
        children: [
          jsx(Row, {
            label: 'New task',
            description: `${board.rows.length} on the board`,
            error: board.error,
            children: jsxs('form', {
              className: 'flex gap-2',
              onSubmit: (e) => {
                e.preventDefault()
                void add().catch(() => {})
              },
              children: [
                jsx(Input, { value: title, onChange: (e) => setTitle(e.target.value), placeholder: 'What needs doing', 'aria-label': 'New task' }),
                jsx(Button, { type: 'submit', size: 'sm', disabled: board.pending, children: 'Add' }),
              ],
            }),
          }),
          jsx(Row, {
            label: 'Ask the agent',
            description: 'an agent whose tools select the board surface reads the same documents',
            children: jsx(Button, {
              size: 'sm',
              variant: 'outline',
              onClick: () => void ctx.send('Read the board (list_tasks and list_moves) and tell me what to run next.'),
              children: 'What next?',
            }),
          }),
        ],
      }),
      jsx('div', {
        className: 'gents-board',
        children: LANES.map((lane) =>
          jsx(Lane, {
            lane,
            tasks: board.rows.filter((t) => t.lane === lane),
            highlight,
            onMove: (id, to) => void board.move(id, to).catch(() => {}),
          }, lane),
        ),
      }),
    ],
  })
}

export default {
  id: 'board',
  name: 'Board',
  description: 'Task board: its own Board page, a nav row that opens it, and a ::board card; the board_docs pack holds the documents it reads and writes.',
  /* ships off, as the kanban plugin does: it inventories, and registers
     once the operator turns it on */
  defaultEnabled: false,
  register(ctx) {
    ctx.registerMany([
      {
        id: 'nav',
        area: NAV_AREA,
        order: 60,
        data: {
          id: 'board:nav',
          label: 'Board',
          icon: 'kanban',
          /* the plugin's own page, not the agent's configuration */
          to: { name: 'plugin', pluginId: 'board' },
          placement: 'footer',
          order: 60,
        },
      },
      {
        id: 'page',
        area: PLUGIN_PAGE_AREA,
        render: () => jsx(BoardPage, { ctx }),
      },
      {
        /* the ::board{id="…"} card: a chip the agent drops into chat that
           opens the page on that task; the id is model output, validated
           before it can select anything */
        id: 'directive',
        area: TRANSCRIPT_DIRECTIVE_AREA,
        data: {
          name: 'board',
          label: 'Board · task #{id}',
          onClick: (attrs) => {
            highlightTask = TASK_ID.test(attrs.id ?? '') ? attrs.id : null
            ctx.shell.navigate({ name: 'plugin', pluginId: 'board' })
          },
        },
      },
    ])
    ctx.onDispose(() => toast('Board plugin unloaded'))
  },
}
