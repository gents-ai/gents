/* One line per tool call, in words a person would use: the desktop's
   ToolGroup summary, as data so the transcript steps and the trace can
   both label a call the same way. */
import type { RenderedToolCallView, ToolPresentationView } from '@source-inc/gents-desktop-client'

const compact = (value: string | null | undefined, max = 80) => {
  const flat = value?.replace(/\s+/g, ' ').trim()
  if (!flat) return null
  return flat.length > max ? `${flat.slice(0, max)}…` : flat
}
export const duration = (ms: number) => {
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1).replace(/\.0$/, '')}s`
  const m = Math.floor(ms / 60_000)
  const s = Math.round((ms % 60_000) / 1000)
  return s ? `${m}m ${s}s` : `${m}m`
}
const exit = (p: Extract<ToolPresentationView, { kind: 'command' }>) =>
  p.timedOut ? 'timed out' : p.exitCode != null ? `exit ${p.exitCode}` : p.failed ? 'failed' : null

const readCount = (p: Extract<ToolPresentationView, { kind: 'fileRead' }>) => {
  if (p.returnedCount == null) return null
  const total =
    p.totalCount != null && p.totalCount !== p.returnedCount ? ` of ${p.totalCount}` : ''
  return `${p.returnedCount}${total}${p.truncated ? ' · truncated' : ''}`
}

/* one line: what the tool did, in words a person would use */
/* a policy denial reads as what was refused, not as a failed tool */
export function toolSummary(t: RenderedToolCallView): {
  kind: string
  primary: string
  secondary?: string | null
  mono?: boolean
} {
  const p = t.presentation
  if (t.denial && t.statusKind === 'error')
    return {
      kind: `denied · ${t.denial.categoryLabel}`,
      primary: t.denial.deniedCommand ?? t.toolName,
      secondary: t.denial.reasonLine,
      mono: Boolean(t.denial.deniedCommand),
    }
  switch (p.kind) {
    case 'command':
      return { kind: '$', primary: p.command, secondary: exit(p), mono: true }
    case 'fileRead':
      return {
        kind: p.operation.replace('_file', ''),
        primary: p.target ?? t.toolName,
        secondary: readCount(p),
        mono: true,
      }
    case 'fileEdit': {
      const verb =
        p.created === true
          ? 'created'
          : p.created === false && p.operation === 'write_file'
            ? 'overwrote'
            : t.statusKind === 'running'
              ? p.operation === 'write_file'
                ? 'writing'
                : 'editing'
              : 'edited'
      return {
        kind: verb,
        primary: p.path ?? t.toolName,
        secondary:
          p.replacementsApplied != null && p.replacementsApplied > 1
            ? `×${p.replacementsApplied}`
            : null,
        mono: true,
      }
    }
    case 'subagent':
      return {
        kind: `subagent · ${p.action}`,
        primary: p.name ?? p.childRequestId ?? 'subagent',
        secondary: compact(p.description),
      }
    case 'process':
      return { kind: `process · ${p.action}`, primary: p.target ?? 'background work', mono: true }
    case 'mcp':
      return { kind: 'MCP', primary: p.selectedToolName ?? t.toolName, secondary: p.serviceId }
    default:
      return { kind: '', primary: t.toolName, secondary: compact(p.summary) }
  }
}
