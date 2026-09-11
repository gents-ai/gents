/* A conversation that is loading, blocked or failed, with the one action
   the desktop offers for it: try again, reconnect, or configure
   inference. Nothing here is invented; the projection is the desktop's. */
import { useState } from 'react'
import { Button } from '@gents/ui/components/button'
import { Spinner } from '@gents/ui/components/spinner'
import { cn } from '@gents/ui/lib/utils'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { toast } from 'sonner'

const LABEL = {
  retryLocal: 'Try again',
  retryHydration: 'Try again',
  reconnect: 'Reconnect',
  configureInference: 'Configure inference',
}
const BUSY = {
  retryLocal: 'Retrying…',
  retryHydration: 'Retrying…',
  reconnect: 'Reconnecting…',
  configureInference: 'Opening…',
}

export function LoadingStatus({ shell }: { shell: Shell }) {
  const status = shell.conversationLoading
  const [busy, setBusy] = useState(false)
  if (!status) return null
  const act = async () => {
    const action = status.action
    if (!action) return
    if (action === 'configureInference') {
      const agentDid = shell.selectedAgentDid
      navigate(agentDid ? { name: 'agent', agentDid, section: 'inference' } : { name: 'agents' })
      return
    }
    setBusy(true)
    try {
      if (action === 'reconnect') await shell.reconnect()
      else if (action === 'retryHydration')
        await shell.retrySessionHydration(shell.selectedSessionId)
      else await shell.refreshSnapshot()
    } catch (e) {
      toast(String(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div
      role={status.phase === 'failed' ? 'alert' : 'status'}
      className={cn(
        'mb-3 flex items-center gap-3 rounded-2xl border px-4 py-3',
        status.phase === 'failed'
          ? 'border-destructive/30 bg-destructive/5'
          : 'border-border/60 bg-raised',
      )}
    >
      {status.phase === 'loading' && <Spinner className="text-foreground" />}
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium">{status.title}</p>
        <p className="text-xs text-muted-foreground">{status.detail}</p>
      </div>
      {status.action && (
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void act()}>
          {busy
            ? BUSY[status.action as keyof typeof BUSY]
            : LABEL[status.action as keyof typeof LABEL]}
        </Button>
      )}
    </div>
  )
}
