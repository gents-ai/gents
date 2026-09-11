/* A tool call, shown by what it did rather than by its raw document: the
   same summary and body the desktop's ToolGroup builds from the
   presentation kinds (command, fileRead, fileEdit, subagent, process,
   mcp, generic). Used by the activity steps in the transcript and by the
   trace panel. */
import type { RenderedToolCallView } from '@source-inc/gents-desktop-client'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import { cn } from '@gents/ui/lib/utils'
import { CopyButton } from './Markdown'
import { duration, toolSummary } from './tool-summary'

const pretty = (value: string) => {
  try {
    return JSON.stringify(JSON.parse(value), null, 2)
  } catch {
    return value
  }
}

/* kinds an icon already says on its own: dropped when the row has one */
const SAID_BY_ICON = new Set(['read', 'edited', '$'])

export function ToolSummary({
  tool,
  className,
  withIcon = false,
}: {
  tool: RenderedToolCallView
  className?: string
  /** the row shows a kind icon, so a kind word that only repeats it is left out */
  withIcon?: boolean
}) {
  const s = toolSummary(tool)
  const kind = withIcon && SAID_BY_ICON.has(s.kind) ? '' : s.kind
  return (
    <span className={cn('flex min-w-0 items-baseline gap-2 text-sm', className)}>
      {kind && <span className="shrink-0 text-muted-foreground">{kind}</span>}
      <span className={cn('min-w-0 truncate', s.mono && 'font-mono text-[13px]')}>{s.primary}</span>
      {s.secondary && (
        <span className="min-w-0 max-w-[50%] shrink truncate text-xs text-muted-foreground">
          {s.secondary}
        </span>
      )}
    </span>
  )
}

function Payload({ label, value }: { label: string; value?: string | null }) {
  if (!value?.trim()) return null
  const text = pretty(value)
  return (
    <div className="grid gap-1">
      <div className="flex items-center justify-between">
        <span className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
          {label}
        </span>
        <CopyButton getText={() => text} />
      </div>
      <ScrollArea className="max-h-72 rounded-md bg-surface [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]">
        <pre className="px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-foreground">
          {text}
        </pre>
      </ScrollArea>
    </div>
  )
}

function Meta({ items }: { items: (string | null | undefined)[] }) {
  const shown = items.filter(Boolean) as string[]
  if (!shown.length) return null
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-1 font-mono text-[11px] text-muted-foreground">
      {shown.map((m) => (
        <span key={m}>{m}</span>
      ))}
    </div>
  )
}

/* the body: outputs, contents, the diff, the live tail */
export function ToolBody({ tool }: { tool: RenderedToolCallView }) {
  const p = tool.presentation
  const live = tool.statusKind === 'running' ? tool.partialOutputTail : null
  const denial = tool.statusKind === 'error' ? tool.denial : null
  return (
    <div className="grid gap-3">
      {denial && (
        <>
          <Meta
            items={[
              `rule · ${denial.ruleId}`,
              denial.deniedCommand && `command · ${denial.deniedCommand}`,
              denial.deniedSubcommand && `subcommand · ${denial.deniedSubcommand}`,
              denial.deniedArgument && `argument · ${denial.deniedArgument}`,
            ]}
          />
          <p className="text-sm">{denial.reasonLine}</p>
          <Payload label="diagnostic" value={denial.diagnostic} />
        </>
      )}
      {tool.cancelCause && (
        <Meta
          items={[
            `cancelled · ${tool.cancelCause.cause}`,
            tool.cancelCause.source,
            tool.cancelCause.at ? new Date(tool.cancelCause.at).toLocaleTimeString() : null,
          ]}
        />
      )}
      <Meta
        items={[
          tool.awaitMode && `await: ${tool.awaitMode}`,
          tool.cancelPolicy && `cancel: ${tool.cancelPolicy}`,
          tool.deadlineAt && `deadline: ${tool.deadlineAt}`,
        ]}
      />
      {p.kind === 'command' && (
        <>
          <Meta
            items={[
              p.durationMs != null ? duration(p.durationMs) : null,
              p.cwd,
              p.executionMode && `sandbox: ${p.executionMode}`,
              p.networkMode && `network: ${p.networkMode}`,
            ]}
          />
          <Payload label="stdout" value={p.stdout} />
          <Payload label="stderr" value={p.stderr} />
          <Payload label="output" value={p.fallbackOutput} />
        </>
      )}
      {p.kind === 'fileRead' && (
        <>
          <Payload label="contents" value={p.body} />
          <Payload label="output" value={p.fallbackOutput} />
        </>
      )}
      {p.kind === 'fileEdit' && (
        <>
          {p.diff.length > 0 && (
            <div className="grid gap-1">
              <div className="flex items-center justify-between">
                <span className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
                  diff
                </span>
                <CopyButton
                  getText={() =>
                    p.diff.map((l) => `${l.kind === 'added' ? '+' : '-'}${l.text}`).join('\n')
                  }
                />
              </div>
              <ScrollArea className="max-h-72 rounded-md bg-surface [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]">
                <pre className="py-2 font-mono text-[11px] leading-relaxed">
                  {p.diff.map((l, i) => (
                    <span
                      key={i}
                      className={cn(
                        'block px-3',
                        l.kind === 'added'
                          ? 'bg-success text-marker-foreground'
                          : 'bg-destructive/10 text-muted-foreground line-through decoration-destructive/40',
                      )}
                    >
                      <span aria-hidden="true" className="mr-2 inline-block w-2 select-none">
                        {l.kind === 'added' ? '+' : '-'}
                      </span>
                      {l.text}
                    </span>
                  ))}
                </pre>
              </ScrollArea>
            </div>
          )}
          <Payload label="output" value={p.fallbackOutput} />
        </>
      )}
      {p.kind === 'subagent' && (
        <>
          <Payload
            label={p.action === 'spawn' ? 'assignment' : 'instruction'}
            value={p.description}
          />
          {p.childRequestId && <Meta items={[`child request · ${p.childRequestId}`]} />}
          <Payload label="result" value={p.output} />
        </>
      )}
      {p.kind === 'process' && (
        <>
          <Payload label="arguments" value={p.description} />
          <Payload label="result" value={p.output} />
        </>
      )}
      {p.kind === 'mcp' && (
        <>
          <Payload label="arguments" value={p.arguments} />
          <Payload label="result" value={p.output} />
        </>
      )}
      {p.kind === 'generic' && (
        <>
          <Payload label="input" value={p.input} />
          <Payload label="result" value={p.output} />
        </>
      )}
      {live && (
        <div className="grid gap-1">
          <span className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            live output
          </span>
          <ScrollArea className="max-h-40 rounded-md bg-surface [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]">
            <pre className="px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap">
              {live}
            </pre>
          </ScrollArea>
        </div>
      )}
    </div>
  )
}
