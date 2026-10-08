// Runtime-loaded example: plain ESM with jsx() calls, exactly what an agent
// (or a person) writes into <home>/ux-plugins/<name>/plugin.js. It takes the
// real runtime pipeline: import allowlist -> specifier rewrite to the SDK
// and React shims -> blob import -> register(ctx). Ported from hermes-agent's
// hello-runtime/plugin.runtime.js; the status bar became the session
// header, the only bar area the first wave has.
import { Hint, SESSION_HEADER_ACTIONS_AREA, cn } from '@gents/ux-sdk'
import { jsx, jsxs } from 'react/jsx-runtime'

function RuntimeChip({ ctx }) {
  const route = ctx.shell.route()
  return jsx(Hint, {
    label: `Loaded at runtime through blob import + SDK injection (route: ${route.name})`,
    children: jsxs('span', {
      className: cn('inline-flex h-7 items-center gap-1 px-1.5 text-[11px] text-muted-foreground'),
      children: [jsx('span', { 'aria-hidden': true, children: '⚡' }), jsx('span', { children: 'runtime' })],
    }),
  })
}

export default {
  id: 'hello_runtime',
  name: 'Hello Runtime',
  description: 'A header chip that proves the runtime loader works.',
  register(ctx) {
    ctx.register({
      id: 'chip',
      area: SESSION_HEADER_ACTIONS_AREA,
      order: 110,
      render: () => jsx(RuntimeChip, { ctx }),
    })
  },
}
