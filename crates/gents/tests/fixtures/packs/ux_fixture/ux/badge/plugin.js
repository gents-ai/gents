// A plain ESM UX plugin: no build step, SDK imports only. The shape an
// agent or a person writes into a pack's ux/ folder by hand. A header
// action is data the shell draws; the click reaches this frame.
import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'

export default {
  id: 'badge',
  name: 'Fixture badge',
  register(ctx) {
    ctx.register({
      id: 'chip',
      area: SESSION_HEADER_ACTIONS_AREA,
      order: 50,
      data: { label: 'fixture', title: 'Fixture badge', onClick: () => ctx.storage.set('clicked', true) },
    })
  }
}
