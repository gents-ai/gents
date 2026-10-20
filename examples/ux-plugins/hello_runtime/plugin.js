// Runtime-loaded example: plain ESM, exactly what an agent (or a person)
// writes into <home>/ux-plugins/<name>/plugin.js. It takes the real
// pipeline: import allowlist -> a frame of its own -> register(ctx). A
// header action is data the shell draws; the click runs here, in the
// frame, and reaches the shell only through ctx.
import { SESSION_HEADER_ACTIONS_AREA, toast } from '@gents/ux-sdk'

export default {
  id: 'hello_runtime',
  name: 'Hello Runtime',
  description: 'A header action that proves the runtime loader works.',
  register(ctx) {
    ctx.register({
      id: 'chip',
      area: SESSION_HEADER_ACTIONS_AREA,
      order: 110,
      data: {
        label: 'runtime',
        icon: 'zap',
        title: 'Loaded at runtime into a frame of its own',
        onClick: () => toast.success(`runtime plugin, route: ${ctx.shell.route().name}`),
      },
    })
  },
}
