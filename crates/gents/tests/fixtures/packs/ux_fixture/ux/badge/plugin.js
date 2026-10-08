// A plain ESM UX plugin: no build step, SDK imports only. The shape an
// agent or a person writes into a pack's ux/ folder by hand.
import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'
import { jsx } from 'react/jsx-runtime'

function Badge() {
  return jsx('span', { className: 'ux-fixture-badge', children: 'fixture' })
}

export default {
  id: 'badge',
  name: 'Fixture badge',
  register(ctx) {
    ctx.register({ id: 'chip', area: SESSION_HEADER_ACTIONS_AREA, order: 50, render: () => jsx(Badge, {}) })
  }
}
