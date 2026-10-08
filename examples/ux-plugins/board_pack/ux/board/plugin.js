// Board: the shape of hermes-agent's kanban plugin on Gents. A page under
// the agent (AGENT_SECTIONS_AREA stands in for ROUTES_AREA), a nav row
// (NAV_AREA for SIDEBAR_NAV_AREA), a ::board{id} card the agent can emit
// (TRANSCRIPT_DIRECTIVE_AREA, as in Hermes), and the pack's own
// board_store plugin as the backend half: ctx.plugin() where Hermes uses
// ctx.rest(). Plain ESM, lint-clean, no build.
import {
  AGENT_SECTIONS_AREA,
  Button,
  Group,
  Input,
  NAV_AREA,
  Row,
  TRANSCRIPT_DIRECTIVE_AREA,
  toast,
} from '@gents/ux-sdk'
import { useCallback, useEffect, useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

const LANES = ['triage', 'ready', 'running', 'done']

/* the board's folder: the operator allows one with `gents plugin dirs
   add`; the page remembers the last choice in the plugin's own storage */
function useBoard(ctx) {
  const [root, setRoot] = useState(() => ctx.storage.get('root', ''))
  const [tasks, setTasks] = useState([])
  const [error, setError] = useState(null)
  const call = useCallback(
    async (input) => {
      if (!root) return null
      try {
        const out = await ctx.plugin('board_store', { root, ...input })
        setError(null)
        return out
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e))
        return null
      }
    },
    [ctx, root],
  )
  const reload = useCallback(async () => {
    const out = await call({ action: 'list' })
    if (out && Array.isArray(out.tasks)) setTasks(out.tasks)
  }, [call])
  useEffect(() => {
    void reload()
  }, [reload])
  const chooseRoot = async () => {
    const picked = await ctx.os.pickDirectory({ title: 'Board folder' })
    if (!picked) return
    ctx.storage.set('root', picked)
    setRoot(picked)
  }
  return { root, tasks, error, reload, call, chooseRoot }
}

function Lane({ lane, tasks, onMove, onRemove }) {
  return jsxs('div', {
    className: 'gents-board-lane',
    children: [
      jsxs('h4', { className: 'gents-board-lane-title', children: [lane, ' ', jsx('span', { children: tasks.length })] }),
      ...tasks.map((t) =>
        jsxs('div', {
          className: 'gents-board-card',
          'data-task': t.id,
          children: [
            jsxs('span', { className: 'gents-board-card-title', children: [jsx('code', { children: `#${t.id}` }), ' ', t.title] }),
            jsxs('div', {
              className: 'gents-board-card-actions',
              children: [
                ...LANES.filter((l) => l !== lane).map((l) =>
                  jsx(Button, { size: 'sm', variant: 'ghost', onClick: () => onMove(t.id, l), children: `→ ${l}` }, l),
                ),
                jsx(Button, { size: 'sm', variant: 'ghost', onClick: () => onRemove(t.id), children: '✕' }),
              ],
            }),
          ],
        }, t.id),
      ),
    ],
  })
}

function BoardPage({ ctx }) {
  const board = useBoard(ctx)
  const [title, setTitle] = useState('')
  const add = async () => {
    if (!title.trim()) return
    await board.call({ action: 'add', title: title.trim(), lane: 'triage' })
    setTitle('')
    await board.reload()
  }
  const move = async (id, lane) => {
    await board.call({ action: 'move', id, lane })
    await board.reload()
  }
  const remove = async (id) => {
    await board.call({ action: 'remove', id })
    await board.reload()
  }
  if (!board.root) {
    return jsx(Group, {
      title: 'Board',
      children: jsx(Row, {
        label: 'Board folder',
        description: 'board.json lives in a folder the operator allowed (gents plugin dirs add)',
        children: jsx(Button, { size: 'sm', onClick: () => void board.chooseRoot(), children: 'Choose folder' }),
      }),
    })
  }
  return jsxs('div', {
    children: [
      jsxs(Group, {
        title: 'Board',
        action: jsx(Button, { size: 'sm', variant: 'ghost', onClick: () => void board.chooseRoot(), children: board.root }),
        children: [
          jsx(Row, {
            label: 'New task',
            error: board.error,
            children: jsxs('form', {
              className: 'flex gap-2',
              onSubmit: (e) => {
                e.preventDefault()
                void add()
              },
              children: [
                jsx(Input, { value: title, onChange: (e) => setTitle(e.target.value), placeholder: 'What needs doing', 'aria-label': 'New task' }),
                jsx(Button, { type: 'submit', size: 'sm', children: 'Add' }),
              ],
            }),
          }),
          jsx(Row, {
            label: 'Ask the agent',
            description: 'the agent reads the same board through the board_store tool',
            children: jsx(Button, {
              size: 'sm',
              variant: 'outline',
              onClick: () => void ctx.send(`List the board in ${board.root} and tell me what to run next.`),
              children: 'What next?',
            }),
          }),
        ],
      }),
      jsx('div', {
        className: 'gents-board',
        children: LANES.map((lane) =>
          jsx(Lane, { lane, tasks: board.tasks.filter((t) => t.lane === lane), onMove: move, onRemove: remove }, lane),
        ),
      }),
    ],
  })
}

/* the ::board{id="3"} card: a chip the agent drops into chat that opens
   the page; attrs are untrusted model output, so the id is validated */
function BoardCard({ ctx, attrs }) {
  const id = /^\d{1,9}$/.test(attrs.id ?? '') ? attrs.id : null
  const deployment = ctx.shell.selectedDeployment()
  return jsx(Button, {
    size: 'sm',
    variant: 'outline',
    className: 'gents-board-chip',
    disabled: !deployment,
    onClick: () => deployment && ctx.shell.navigate({ name: 'agent', agentDid: deployment.agentDid, section: 'board:page', item: id ?? undefined }),
    children: id ? `Board · task #${id}` : 'Board',
  })
}

export default {
  id: 'board',
  name: 'Board',
  description: 'Task board: a Board page, nav row, and a ::board card; backed by the pack\'s board_store plugin.',
  register(ctx) {
    ctx.registerMany([
      {
        id: 'nav',
        area: NAV_AREA,
        order: 60,
        data: {
          id: 'board:nav',
          label: 'Board',
          icon: null,
          to: { name: 'agents' },
          active: (route) => route.name === 'agent' && route.section === 'board:page',
          placement: 'footer',
          order: 60,
        },
      },
      {
        id: 'page',
        area: AGENT_SECTIONS_AREA,
        data: { group: 'Packs', label: 'Board', render: () => jsx(BoardPage, { ctx }) },
      },
      {
        id: 'directive',
        area: TRANSCRIPT_DIRECTIVE_AREA,
        data: { name: 'board', render: ({ attrs }) => jsx(BoardCard, { ctx, attrs }) },
      },
    ])
    ctx.onEvent('client-updated', () => {
      /* the agent may have moved a card; a mounted page re-reads on its
         own effect, so nothing to do here but it is where a refresh
         signal would go */
    })
    ctx.onDispose(() => toast('Board plugin unloaded'))
  },
}
