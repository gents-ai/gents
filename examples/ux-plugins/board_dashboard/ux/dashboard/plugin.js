// Board dashboard: a small read-only page over the `board` surface that
// ../../../board_docs installs on the node, assembled only from SDK blocks.
// No stylesheet: Page, Grid, Stat, the charts and Table own their own look
// in both themes. Plain ESM, lint-clean, no build.
import {
  BarChart,
  Button,
  DonutChart,
  EmptyState,
  ErrorState,
  Grid,
  LastUpdated,
  LineChart,
  LoadingState,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  Page,
  Select,
  Stat,
  Table,
  Tabs,
  useQuery,
} from '@gents/ux-sdk'
import { Fragment, useMemo, useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

const SURFACE = 'board'
const LANES = ['triage', 'ready', 'running', 'done']
const ALL_LANES = 'all'
const ARCHIVED = 'archived'

/* every task with its latest move deciding the lane */
function fold(tasks, moves) {
  const latest = new Map()
  for (const m of moves) {
    const prev = latest.get(m.task_id)
    if (!prev || m.moved_at > prev.moved_at) latest.set(m.task_id, m)
  }
  return tasks
    .map((t) => ({ ...t, lane: latest.get(t.task_id)?.lane ?? t.lane }))
    .filter((t) => t.lane !== ARCHIVED)
}

const count = (rows, lane) => rows.filter((t) => t.lane === lane).length

function perDay(rows) {
  const days = new Map()
  for (const t of rows) {
    const day = String(t.created_at).slice(0, 10)
    days.set(day, (days.get(day) ?? 0) + 1)
  }
  return [...days].map(([day, created]) => ({ day, created }))
}

function Dashboard({ ctx }) {
  const tasks = useQuery(ctx, SURFACE, 'list_tasks', { pages: 'all' })
  const moves = useQuery(ctx, SURFACE, 'list_moves', { pages: 'all' })
  const [lane, setLane] = useState(ALL_LANES)
  const rows = useMemo(
    () => fold(tasks.data?.results ?? [], moves.data?.results ?? []),
    [tasks.data, moves.data],
  )
  const refresh = () => {
    tasks.refresh()
    moves.refresh()
  }
  const loading = tasks.loading || moves.loading
  const error = tasks.error ?? moves.error

  const actions = jsxs(Fragment, {
    children: [
      jsx(LastUpdated, { at: tasks.updatedAt, loading }),
      jsx(Button, { size: 'sm', variant: 'outline', onClick: refresh, children: 'Refresh' }),
    ],
  })

  if (loading && !tasks.data) {
    return jsx(Page, { title: 'Board dashboard', children: jsx(LoadingState, { label: 'Loading the board' }) })
  }
  if (error && !tasks.data) {
    return jsx(Page, { title: 'Board dashboard', children: jsx(ErrorState, { message: error, onRetry: refresh }) })
  }
  if (rows.length === 0) {
    return jsx(Page, {
      title: 'Board dashboard',
      actions,
      children: jsx(EmptyState, { title: 'No tasks yet', message: 'Add a task to the board and it shows up here.' }),
    })
  }

  const byLane = LANES.map((name) => ({ lane: name, tasks: count(rows, name) }))
  const visible = lane === ALL_LANES ? rows : rows.filter((t) => t.lane === lane)
  const created = perDay(rows)
  const open = rows.length - count(rows, 'done')

  const overview = jsxs(Grid, {
    minWidth: 360,
    children: [
      jsx(LineChart, { title: 'Tasks created', data: created, x: 'day', y: 'created' }),
      jsx(BarChart, { title: 'Tasks by lane', data: byLane, x: 'lane', y: 'tasks' }),
      jsx(DonutChart, { title: 'Share of the board', data: byLane, label: 'lane', value: 'tasks' }),
    ],
  })
  const table = jsxs(Fragment, {
    children: [
      jsx(Select, {
        label: 'Lane',
        value: lane,
        onChange: setLane,
        options: [{ value: ALL_LANES, label: 'All lanes' }, ...LANES],
      }),
      jsx(Table, {
        title: 'Tasks',
        rowKey: 'task_id',
        rows: visible,
        columns: [
          { key: 'title', label: 'Task' },
          { key: 'lane', label: 'Lane' },
          { key: 'created_at', label: 'Created' },
        ],
        defaultSort: { key: 'created_at', direction: 'desc' },
        empty: 'No tasks in this lane',
      }),
    ],
  })

  return jsxs(Page, {
    title: 'Board dashboard',
    description: 'What is on the board right now',
    actions,
    children: [
      jsxs(Grid, {
        children: [
          jsx(Stat, { label: 'Open tasks', value: open }),
          jsx(Stat, { label: 'Running', value: count(rows, 'running') }),
          jsx(Stat, { label: 'Done', value: count(rows, 'done'), tone: 'good', delta: `${Math.round((count(rows, 'done') / rows.length) * 100)}% of the board` }),
        ],
      }),
      jsx(Tabs, {
        items: [
          { value: 'overview', label: 'Overview', content: overview },
          { value: 'tasks', label: 'Tasks', content: table },
        ],
      }),
    ],
  })
}

export default {
  id: 'board_dashboard',
  name: 'Board dashboard',
  description: 'A read-only dashboard over the board, built from SDK blocks.',
  register(ctx) {
    ctx.registerMany([
      { id: 'nav', area: NAV_AREA, order: 81, data: {
        id: 'board_dashboard:nav', label: 'Dashboard', icon: 'puzzle',
        to: { name: 'plugin', pluginId: 'board_dashboard' },
        placement: 'footer', order: 81 } },
      { id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(Dashboard, { ctx }) },
    ])
  },
}
