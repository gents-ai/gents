// Focus timer: a header action whose label is the clock, a page of its
// own with the presets, state the plugin owns and persists through
// ctx.storage, a timer that cannot outlive the plugin (the frame goes with
// a deactivation), and a stylesheet beside the module. Plain ESM; no
// build step. The header action is data the shell draws: the plugin
// re-registers it each second with the label the clock reads.
import { Button, Group, NAV_AREA, PLUGIN_PAGE_AREA, Row, SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'
import { useEffect, useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

const PRESETS = [
  { id: 'short', label: 'Short focus', minutes: 15 },
  { id: 'standard', label: 'Standard', minutes: 25 },
  { id: 'deep', label: 'Deep work', minutes: 50 },
]

const DEFAULT_STATE = { presetId: 'standard', endsAt: null, done: 0, ended: false }

/* what this plugin wrote and nothing else: storage is the plugin's own,
   but a hand edit or an older version must not become a throw or a
   block that ends on launch */
function readState(stored) {
  if (typeof stored !== 'object' || stored === null) return DEFAULT_STATE
  const presetId = PRESETS.some((p) => p.id === stored.presetId) ? stored.presetId : 'standard'
  const endsAt = Number.isFinite(stored.endsAt) && stored.endsAt > 0 ? stored.endsAt : null
  const done = Number.isInteger(stored.done) && stored.done >= 0 ? stored.done : 0
  const ended = stored.ended === true
  /* a block that ran out while the app was closed is counted, quietly:
     the question to the agent waits for the operator's click */
  if (endsAt !== null && endsAt <= Date.now()) {
    return { presetId, endsAt: null, done: done + 1, ended: true }
  }
  return { presetId, endsAt, done, ended }
}

/* a tiny store: one object, a set of listeners, persisted on every write */
function createTimer(ctx) {
  const listeners = new Set()
  let state = readState(ctx.storage.get('state', null))
  /* written back at once, so a block settled here is not counted again
     on the next launch */
  ctx.storage.set('state', state)
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
      /* the block is over; ctx.send needs the operator's own click, so the
         page offers the question rather than asking it from a timer */
      set({ endsAt: null, done: state.done + 1, ended: true })
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
    start: () => set({ endsAt: Date.now() + preset().minutes * 60_000, ended: false }),
    stop: () => set({ endsAt: null }),
    choose: (presetId) => set({ presetId, endsAt: null }),
    /* from the page's button: a user gesture, as ctx.send requires */
    askNext: () => {
      set({ ended: false })
      return ctx.send(`Focus block done (${preset().label}, ${preset().minutes} min). What should I do next?`)
    },
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

function Page({ timer }) {
  useTimer(timer)
  const state = timer.get()
  const running = state.endsAt !== null
  return jsxs(Group, {
    title: running ? clock(timer.remaining()) : timer.preset().label,
    children: [
      jsx(Row, {
        label: 'Preset',
        description: 'the length of one block',
        children: jsx('div', {
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
      }),
      jsx(Row, {
        label: running ? 'Running' : 'Stopped',
        description: `${state.done} block${state.done === 1 ? '' : 's'} done`,
        children: jsx(Button, {
          size: 'sm',
          onClick: () => (running ? timer.stop() : timer.start()),
          children: running ? 'Stop' : 'Start',
        }),
      }),
      state.ended
        ? jsx(Row, {
            label: 'Block done',
            description: 'ask the agent what is next',
            children: jsx(Button, {
              size: 'sm',
              variant: 'outline',
              onClick: () => void timer.askNext(),
              children: "Ask what's next",
            }),
          })
        : null,
    ],
  })
}

export default {
  id: 'focus_timer',
  name: 'Focus timer',
  description: 'A focus timer in the session header; when a block ends, one click asks the agent what is next.',
  /* ships off */
  defaultEnabled: false,
  register(ctx) {
    const timer = createTimer(ctx)
    ctx.register({
      id: 'nav',
      area: NAV_AREA,
      order: 70,
      data: { id: 'focus_timer:nav', label: 'Focus', icon: 'timer', to: { name: 'plugin', pluginId: 'focus_timer' }, placement: 'footer', order: 70 },
    })
    ctx.register({ id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(Page, { timer }) })
    /* the clock in the pane bar: the same id each second replaces the row */
    const chip = () => {
      const running = timer.get().endsAt !== null
      ctx.register({
        id: 'chip',
        area: SESSION_HEADER_ACTIONS_AREA,
        order: 40,
        data: {
          label: running ? clock(timer.remaining()) : 'focus',
          icon: running ? null : 'timer',
          title: running ? 'Focus block running; click to stop' : 'Start a focus block',
          onClick: () => (running ? timer.stop() : timer.start()),
        },
      })
    }
    chip()
    timer.subscribe(chip)
  },
}
