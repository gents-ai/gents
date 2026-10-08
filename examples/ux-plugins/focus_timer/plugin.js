// Focus timer: a port of the shape of hermes-agent's radio plugin (a
// header chip that opens a popover, state the plugin owns and persists
// through ctx.storage, a timer that cannot outlive the plugin because it
// is taken out through ctx.setInterval, and a stylesheet beside the
// module). Plain ESM; no build step.
import {
  Button,
  Hint,
  Popover,
  PopoverContent,
  PopoverTrigger,
  SESSION_HEADER_ACTIONS_AREA,
} from '@gents/ux-sdk'
import { useEffect, useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

const PRESETS = [
  { id: 'short', label: 'Short focus', minutes: 15 },
  { id: 'standard', label: 'Standard', minutes: 25 },
  { id: 'deep', label: 'Deep work', minutes: 50 },
]

/* a tiny store: one object, a set of listeners, persisted on every write.
   The radio plugin uses nanostores atoms for this; the SDK ships none, so a
   closure over ctx.storage does the same job in ten lines. */
function createTimer(ctx) {
  const listeners = new Set()
  let state = ctx.storage.get('state', { presetId: 'standard', endsAt: null, done: 0 })
  const notify = () => listeners.forEach((fn) => fn(state))
  const set = (patch) => {
    state = { ...state, ...patch }
    ctx.storage.set('state', state)
    notify()
  }
  const preset = () => PRESETS.find((p) => p.id === state.presetId) ?? PRESETS[1]
  const remaining = () => (state.endsAt ? Math.max(0, state.endsAt - Date.now()) : 0)
  ctx.setInterval(() => {
    if (state.endsAt && remaining() === 0) {
      set({ endsAt: null, done: state.done + 1 })
      void ctx.send(`Focus block done (${preset().label}, ${preset().minutes} min). What should I do next?`)
    } else if (state.endsAt) {
      notify()
    }
  }, 1000)
  return {
    subscribe: (fn) => {
      listeners.add(fn)
      return () => listeners.delete(fn)
    },
    get: () => state,
    preset,
    remaining,
    start: () => set({ endsAt: Date.now() + preset().minutes * 60_000 }),
    stop: () => set({ endsAt: null }),
    choose: (presetId) => set({ presetId, endsAt: null }),
  }
}

function useTimer(timer) {
  const [, tick] = useState(0)
  useEffect(() => timer.subscribe(() => tick((n) => n + 1)), [timer])
  return timer
}

function clock(ms) {
  const total = Math.ceil(ms / 1000)
  return `${String(Math.floor(total / 60)).padStart(2, '0')}:${String(total % 60).padStart(2, '0')}`
}

function Chip({ timer }) {
  useTimer(timer)
  const running = timer.get().endsAt !== null
  return jsxs(Popover, {
    children: [
      jsx(PopoverTrigger, {
        render: jsx(Button, {
          variant: 'ghost',
          size: 'sm',
          className: 'gents-focus-chip',
          'data-running': running,
          'aria-label': 'Focus timer',
          children: running ? clock(timer.remaining()) : '◷ focus',
        }),
      }),
      jsx(PopoverContent, { className: 'gents-focus-panel', children: jsx(Panel, { timer }) }),
    ],
  })
}

function Panel({ timer }) {
  useTimer(timer)
  const state = timer.get()
  const running = state.endsAt !== null
  return jsxs('div', {
    className: 'gents-focus-stack',
    children: [
      jsx('p', { className: 'gents-focus-title', children: running ? clock(timer.remaining()) : timer.preset().label }),
      jsx('div', {
        className: 'gents-focus-presets',
        children: PRESETS.map((p) =>
          jsx(Button, {
            size: 'sm',
            variant: p.id === state.presetId ? 'default' : 'outline',
            disabled: running,
            onClick: () => timer.choose(p.id),
            children: `${p.minutes} min`,
          }, p.id),
        ),
      }),
      jsx(Button, {
        size: 'sm',
        onClick: () => (running ? timer.stop() : timer.start()),
        children: running ? 'Stop' : 'Start',
      }),
      jsx('p', { className: 'gents-focus-note', children: `${state.done} block${state.done === 1 ? '' : 's'} done · the agent is told when one ends` }),
    ],
  })
}

export default {
  id: 'focus_timer',
  name: 'Focus timer',
  description: 'A focus timer in the session header that tells the agent when a block ends.',
  register(ctx) {
    const timer = createTimer(ctx)
    ctx.register({
      id: 'chip',
      area: SESSION_HEADER_ACTIONS_AREA,
      order: 40,
      render: () => jsx(Hint, { label: 'Focus timer', children: jsx(Chip, { timer }) }),
    })
  },
}
