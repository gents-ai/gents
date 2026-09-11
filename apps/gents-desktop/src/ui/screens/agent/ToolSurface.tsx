/* What this behaviour actually gets: the bridge's tool surface
   explanation (selection ∩ ceiling, with reasons), loaded when opened
   and refreshed on demand, as the desktop's BehaviorToolSurface does. */
import { useState } from 'react'
import { ChevronDown, RefreshCw } from 'lucide-react'
import type { ToolSurfaceExplanationView } from '@source-inc/gents-desktop-client'
import { Badge } from '@gents/ui/components/badge'
import { Button } from '@gents/ui/components/button'
import { Spinner } from '@gents/ui/components/spinner'
import { cn } from '@gents/ui/lib/utils'
import type { Shell } from '@/hooks/useShell'
import { Group, Row } from './rows'

const strings = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string' && !!x) : []
const groups = (v: unknown): [string, string[]][] =>
  v && typeof v === 'object'
    ? Object.entries(v as Record<string, unknown>).map(([k, r]) => [k, strings(r)])
    : []

export function ToolSurface({
  shell,
  agentDid,
  behaviorId,
}: {
  shell: Shell
  agentDid: string
  behaviorId: string
}) {
  const [open, setOpen] = useState(false)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [explanation, setExplanation] = useState<ToolSurfaceExplanationView | null>(null)
  const load = async () => {
    setLoading(true)
    setError(null)
    try {
      setExplanation(await shell.api.explainToolSurface(agentDid, behaviorId))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }
  const toggle = () => {
    if (!open && !explanation && !loading) void load()
    setOpen(!open)
  }
  const s = explanation?.surface ?? {}
  return (
    <Group
      title="Resolved tools"
      action={
        <span className="flex items-center gap-2">
          {open && (
            <Button
              variant="quiet"
              size="icon-xs"
              aria-label="Refresh"
              disabled={loading}
              onClick={() => void load()}
            >
              {loading ? <Spinner /> : <RefreshCw />}
            </Button>
          )}
          <Button variant="outline" size="sm" aria-expanded={open} onClick={toggle}>
            {open ? 'Hide' : 'Show'}
            <ChevronDown className={cn('size-3.5 transition-transform', open && 'rotate-180')} />
          </Button>
        </span>
      }
    >
      {!open ? (
        <Row
          label="What this behaviour actually gets"
          description="The selection intersected with the agent's ceiling, with reasons for anything missing."
        />
      ) : error ? (
        <Row label="Explanation failed">
          <span className="text-sm text-destructive">{error}</span>
        </Row>
      ) : !explanation ? (
        <Row label="Resolving…">
          <Spinner />
        </Row>
      ) : (
        <>
          <Row label="Policy">
            <span className="font-mono text-xs text-muted-foreground">
              ceiling: {explanation.ceilingSource} · tools: {explanation.toolsSource} · MCP{' '}
              {explanation.mcpServicesOnline ? 'online' : 'offline'}
            </span>
          </Row>
          <Row
            label="Available"
            description={
              strings(s.tool_names).length
                ? undefined
                : 'None: the intersection with the ceiling is empty.'
            }
          >
            <span className="flex max-w-xl flex-wrap justify-end gap-1">
              {strings(s.tool_names).map((n) => (
                <Badge key={n} variant="secondary" className="font-mono">
                  {n}
                </Badge>
              ))}
            </span>
          </Row>
          {groups(s.excluded).length > 0 && (
            <Row label="Excluded">
              <Reasons groups={groups(s.excluded)} />
            </Row>
          )}
          {groups(s.unavailable).length > 0 && (
            <Row label="Unavailable right now">
              <Reasons groups={groups(s.unavailable)} />
            </Row>
          )}
          {strings(
            (s.warnings as unknown[] | undefined)?.map((w) =>
              w && typeof w === 'object'
                ? String((w as { message?: unknown }).message ?? '')
                : String(w),
            ),
          ).map((m) => (
            <Row key={m} label="Warning">
              <span className="max-w-xl text-right text-xs text-muted-foreground">{m}</span>
            </Row>
          ))}
        </>
      )}
    </Group>
  )
}

function Reasons({ groups: list }: { groups: [string, string[]][] }) {
  return (
    <ul className="grid max-w-xl gap-1 text-right text-xs text-muted-foreground">
      {list.flatMap(([category, reasons]) =>
        reasons.map((r) => (
          <li key={`${category}-${r}`}>
            <span className="mr-1 font-mono text-[10px] uppercase">{category}</span>
            {r}
          </li>
        )),
      )}
    </ul>
  )
}
