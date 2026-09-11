/* The local agent's server, as the desktop supervises it: its state,
   whether it starts with the app, and Start / Stop. Only the local
   deployment has one; peers run their own. */
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import type { ManagedServerStatus } from '@source-inc/gents-desktop-client'
import { Badge } from '@gents/ui/components/badge'
import { Button } from '@gents/ui/components/button'
import { Spinner } from '@gents/ui/components/spinner'
import { Switch } from '@gents/ui/components/switch'
import type { Shell } from '@/hooks/useShell'
import { Fact, Group, Row } from './rows'

export function LocalServer({ shell }: { shell: Shell }) {
  const api = shell.api
  const [status, setStatus] = useState<ManagedServerStatus | null>(null)
  const [busy, setBusy] = useState(false)
  const load = () => api.managedServerStatus?.().then(setStatus, () => setStatus(null))
  useEffect(() => {
    void load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shell.snapshot])
  if (!api.managedServerStatus) return null
  const act = async (label: string, run: () => Promise<ManagedServerStatus> | undefined) => {
    setBusy(true)
    try {
      const next = await run()
      if (next) setStatus(next)
      await shell.refreshSnapshot()
      toast(label)
    } catch (e) {
      toast(`${label} failed: ${String(e)}`)
    } finally {
      setBusy(false)
    }
  }
  const running = status?.state === 'running' || status?.state === 'external'
  const name = status?.agentName ?? 'gents'
  return (
    <Group
      title="Local server"
      action={
        running ? (
          <Button
            size="sm"
            variant="outline"
            disabled={busy || status?.state === 'external'}
            onClick={() => void act('Server stopped', () => api.stopManagedServer?.(false))}
          >
            {busy ? <Spinner /> : null} Stop
          </Button>
        ) : (
          <Button
            size="sm"
            variant="brand"
            disabled={busy || status?.state === 'starting'}
            onClick={() => void act('Server started', () => api.startManagedServer?.(name))}
          >
            {busy || status?.state === 'starting' ? <Spinner /> : null} Start
          </Button>
        )
      }
    >
      <Row
        label="State"
        description="Supervised by the desktop; external means something else runs it."
      >
        <span className="flex items-center gap-2">
          {status?.error && <span className="text-xs text-destructive">{status.error}</span>}
          <Badge
            variant={running ? 'secondary' : status?.state === 'failed' ? 'destructive' : 'outline'}
          >
            {status?.state ?? 'unknown'}
          </Badge>
        </span>
      </Row>
      <Row label="Start with the app" description="Auto-start the server when the desktop opens.">
        <Switch
          checked={status?.autoStart ?? false}
          disabled={busy || !status}
          onCheckedChange={(on) =>
            void act(on ? 'Auto-start on' : 'Auto-start off', () =>
              on ? api.commitManagedServerAutoStart?.(name) : api.stopManagedServer?.(true),
            )
          }
        />
      </Row>
      <Row label="GraphQL">
        <Fact mono>{status?.graphql ?? '—'}</Fact>
      </Row>
    </Group>
  )
}
