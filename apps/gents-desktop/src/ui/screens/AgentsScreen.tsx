/* Agents, the fleet: every deployment this desktop knows. A card per
   agent from the Fleet design: picture with a status dot, name, how many
   runs are live, and an overflow menu with what the desktop's fleet row
   offers (rename the saved label, check the peer, remove). Add agent is
   the desktop's status enrolment: a server address, a request the
   server's admin approves, then the peer joins. A Network section at the
   foot shows this node and repairs P2P. */
import { useState } from 'react'
import { ChevronDown, EllipsisVertical, Inbox, Plus } from 'lucide-react'
import { toast } from 'sonner'
import type { NetworkStatusView } from '@source-inc/gents-desktop-client'
import { Button } from '@gents/ui/components/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@gents/ui/components/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@gents/ui/components/dropdown-menu'
import { Input } from '@gents/ui/components/input'
import { Spinner } from '@gents/ui/components/spinner'
import { cn } from '@gents/ui/lib/utils'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import type { Shell } from '@/hooks/useShell'
import { href } from '@/lib/router'
import { isLive } from '@/lib/live'
import { AgentAvatar } from './AgentAvatar'
import { AgentHoverCard } from './HoverCards'
import { CopyButton } from './Markdown'
import { Fact, Group, Row } from './agent/rows'

export function AgentsScreen({ shell }: { shell: Shell }) {
  const [adding, setAdding] = useState(false)
  const [renaming, setRenaming] = useState<{ peerId: string; label: string } | null>(null)
  const pending = shell.snapshot?.client?.enrollmentRequests
  return (
    <ScrollArea className="h-full">
      <div className="mx-auto max-w-2xl px-6 py-8">
        <div className="flex h-10 items-center justify-between">
          <h1 className="font-heading text-lg font-medium text-heading">Agents</h1>
          <Button variant="secondary" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Add agent
          </Button>
        </div>
        {pending === null && (
          <p className="mt-4 rounded-2xl border border-border/60 bg-raised px-5 py-4 text-sm text-muted-foreground">
            Waiting for the signed enrolment state. New enrolment is disabled until the database can
            be read.
          </p>
        )}
        {pending && pending.length > 0 && (
          <ul className="mt-4 grid gap-3">
            {pending.map((r) => (
              <li
                key={r.requestId}
                className="flex items-center gap-3 rounded-2xl border border-dashed border-border px-5 py-4"
              >
                <Spinner className="text-foreground" />
                <div className="min-w-0">
                  <p className="text-sm font-medium">
                    {r.state === 'approved'
                      ? 'Approval received · finishing secure route'
                      : `Waiting for ${r.serverLabel ?? r.serverPeer} to accept`}
                  </p>
                  <p className="truncate font-mono text-[11px] text-muted-foreground">
                    {r.requestId} · expires {new Date(r.expiresAt).toLocaleTimeString()}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        )}
        <ul className="mt-4 grid gap-3">
          {shell.deployments.map((d) => {
            const name = d.agentPrincipal.displayName ?? d.label
            const online = d.dialSucceeded
            const live = d.sessions.filter((c) => isLive(c.turnState)).length
            const waiting = d.mailboxItems.filter((m) => m.status === 'open').length
            const config = href({ name: 'agent', agentDid: d.agentDid, section: 'agent' })
            const check = async () => {
              try {
                const r = (await shell.api.fetchPeerStatus(d.peerId)) as { reachable?: boolean }
                toast(r?.reachable ? `${d.label} is reachable` : `${d.label} is not reachable`)
              } catch (e) {
                toast(`Status check failed: ${String(e)}`)
              }
            }
            return (
              <li
                key={d.agentDid}
                className="flex items-center gap-4 rounded-2xl border border-border/60 bg-raised px-5 py-4 transition-colors hover:border-border hover:bg-accent"
              >
                <a
                  href={href({ name: 'sessions' })}
                  onClick={() => shell.selectAgent(d.agentDid)}
                  aria-label={`${name} sessions`}
                  className="flex min-w-0 flex-1 items-center gap-3"
                >
                  <AgentHoverCard
                    deployment={d}
                    root={shell.snapshot?.bootstrap.initToolRoot}
                    ceiling={shell.snapshot?.bootstrap.initToolCeiling}
                  >
                    <span className="block">
                      <AgentAvatar name={name} className="size-8" />
                    </span>
                  </AgentHoverCard>
                  <span
                    className={cn(
                      'size-2 shrink-0 rounded-full',
                      online ? 'bg-brand' : 'bg-destructive',
                    )}
                    aria-label={online ? 'online' : 'offline'}
                    role="img"
                  />
                  <span className="truncate font-heading text-base font-medium text-heading">
                    {name}
                  </span>
                  {d.label !== name && (
                    <span className="truncate text-xs text-muted-foreground">{d.label}</span>
                  )}
                  {!online && d.lastError && (
                    <span className="truncate text-xs text-muted-foreground">{d.lastError}</span>
                  )}
                </a>
                {d.source === 'local' && d.inferenceBackends.length === 0 && (
                  <Button
                    size="sm"
                    variant="outline"
                    nativeButton={false}
                    render={
                      <a
                        href={href({ name: 'agent', agentDid: d.agentDid, section: 'inference' })}
                      />
                    }
                    title={`Configure inference for ${d.label}`}
                  >
                    Setup needed
                  </Button>
                )}
                {waiting > 0 && (
                  <a
                    href={href({ name: 'mailbox' })}
                    onClick={() => shell.selectAgent(d.agentDid)}
                    className="flex items-center gap-1.5 rounded-full px-2 py-1 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                    title={`${waiting} item${waiting === 1 ? '' : 's'} need${waiting === 1 ? 's' : ''} your attention`}
                  >
                    <Inbox className="size-4" /> {waiting}
                  </a>
                )}
                {live > 0 && (
                  <span
                    className="flex items-center gap-2 text-sm text-muted-foreground"
                    title={`${live} running`}
                  >
                    <Spinner className="text-foreground" /> {live}
                  </span>
                )}
                <DropdownMenu>
                  <DropdownMenuTrigger
                    render={
                      <Button variant="quiet" size="icon-sm" aria-label={`${name} actions`} />
                    }
                  >
                    <EllipsisVertical />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuGroup>
                      <DropdownMenuItem
                        render={<a href={href({ name: 'sessions' })} />}
                        onClick={() => shell.selectAgent(d.agentDid)}
                      >
                        Open sessions
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        render={<a href={config} />}
                        onClick={() => shell.selectAgent(d.agentDid)}
                      >
                        Configure
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onClick={() => setRenaming({ peerId: d.peerId, label: d.label })}
                      >
                        Rename
                      </DropdownMenuItem>
                      <DropdownMenuItem onClick={() => void check()}>Check peer</DropdownMenuItem>
                    </DropdownMenuGroup>
                    {d.source !== 'local' && (
                      <>
                        <DropdownMenuSeparator />
                        <DropdownMenuGroup>
                          <DropdownMenuItem
                            variant="destructive"
                            onClick={() => {
                              if (confirm(`Remove ${d.label} from this desktop?`))
                                void shell.removePeer(d.peerId).then(() => toast('Peer removed'))
                            }}
                          >
                            Remove peer
                          </DropdownMenuItem>
                        </DropdownMenuGroup>
                      </>
                    )}
                  </DropdownMenuContent>
                </DropdownMenu>
              </li>
            )
          })}
        </ul>
        <Network shell={shell} />
      </div>
      <AddAgentDialog shell={shell} open={adding} onClose={() => setAdding(false)} />
      <RenameDialog
        key={renaming?.peerId ?? 'none'}
        shell={shell}
        target={renaming}
        onClose={() => setRenaming(null)}
      />
    </ScrollArea>
  )
}

/* the desktop's status enrolment: an address, a request, the admin approves */
function AddAgentDialog({
  shell,
  open,
  onClose,
}: {
  shell: Shell
  open: boolean
  onClose: () => void
}) {
  const [address, setAddress] = useState('')
  const [busy, setBusy] = useState(false)
  const ready = /\S/.test(address)
  const submit = async () => {
    setBusy(true)
    try {
      const r = await shell.api.requestStatusEnrollment(address.trim())
      await shell.refreshSnapshot()
      toast(`Enrolment request ${r.requestId} sent · waiting for acceptance`)
      setAddress('')
      onClose()
    } catch (e) {
      toast(`Couldn't request enrolment: ${String(e)}`)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Connect by server address</DialogTitle>
          <DialogDescription>
            Enter an agent's IP address, hostname or URL. Gents reads its status offer,
            authenticates the server and requests enrolment. The server must approve the request
            before chat opens.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-2"
          onSubmit={(e) => {
            e.preventDefault()
            if (ready && !busy) void submit()
          }}
        >
          <label htmlFor="enrol-address" className="text-sm">
            Agent server
          </label>
          <Input
            id="enrol-address"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="100.69.4.79:9191"
            className="font-mono"
            disabled={busy}
            autoFocus
          />
        </form>
        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="brand" disabled={!ready || busy} onClick={() => void submit()}>
            {busy ? <Spinner /> : null} {busy ? 'Connecting…' : 'Request enrolment'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* the saved label for a peer, the one thing the fleet row edits in place */
function RenameDialog({
  shell,
  target,
  onClose,
}: {
  shell: Shell
  target: { peerId: string; label: string } | null
  onClose: () => void
}) {
  const [label, setLabel] = useState(target?.label ?? '')
  const save = async () => {
    if (!target) return
    const next = label.trim()
    if (next && next !== target.label) {
      await shell.renamePeer(target.peerId, next)
      toast('Renamed')
    }
    onClose()
  }
  return (
    <Dialog open={target !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Rename deployment</DialogTitle>
          <DialogDescription>
            The saved label on this desktop; the agent's own name does not change.
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault()
            void save()
          }}
        >
          <Input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus />
        </form>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="brand" onClick={() => void save()}>
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* this node: peer id, listen addresses, connections, saved peers, repair */
function Network({ shell }: { shell: Shell }) {
  const [open, setOpen] = useState(false)
  const [status, setStatus] = useState<NetworkStatusView | null>(null)
  const [loading, setLoading] = useState(false)
  const [repairing, setRepairing] = useState(false)
  const load = async () => {
    setLoading(true)
    try {
      setStatus(await shell.api.fetchNetworkStatus())
    } catch (e) {
      toast(`Network status failed: ${String(e)}`)
    } finally {
      setLoading(false)
    }
  }
  const repair = async () => {
    setRepairing(true)
    try {
      await shell.api.repairP2P()
      await shell.refreshSnapshot()
      await load()
      toast('P2P repaired')
    } catch (e) {
      toast(`Repair failed: ${String(e)}`)
    } finally {
      setRepairing(false)
    }
  }
  return (
    <section className="mt-8">
      <button
        type="button"
        aria-expanded={open}
        className="flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
        onClick={() => {
          if (!open && !status) void load()
          setOpen(!open)
        }}
      >
        Network{' '}
        <ChevronDown className={cn('size-3.5 transition-transform', open && 'rotate-180')} />
      </button>
      {open && (
        <div className="mt-3">
          <Group
            title="This node"
            action={
              <span className="flex gap-1">
                <Button variant="quiet" size="sm" disabled={loading} onClick={() => void load()}>
                  {loading ? <Spinner /> : 'Refresh'}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={repairing}
                  onClick={() => void repair()}
                >
                  {repairing ? <Spinner /> : null} Repair P2P
                </Button>
              </span>
            }
          >
            {status ? (
              <>
                <Row label="Peer ID">
                  <Mono
                    lines={[status.localPeerId ?? status.localPeerIdError ?? 'unknown']}
                    copy={status.localPeerId}
                  />
                </Row>
                <Row label="Listening" description="Addresses this node accepts peers on.">
                  <Mono
                    lines={
                      status.listenAddressesError
                        ? [status.listenAddressesError]
                        : status.listenAddresses
                    }
                    copy={status.listenAddresses.join('\n') || null}
                  />
                </Row>
                <Row label="Connected">
                  <Mono
                    lines={
                      status.connectedPeersError
                        ? [status.connectedPeersError]
                        : status.connectedPeers
                    }
                  />
                </Row>
                <Row label="Saved peers" description="Known peers this desktop dials.">
                  <span className="grid max-w-[28rem] gap-1 text-right text-sm">
                    {status.savedPeers.length === 0 && <Fact>none</Fact>}
                    {status.savedPeers.map((p) => (
                      <span key={p.peerId} className="flex items-center justify-end gap-2">
                        <span>{p.label}</span>
                        <span className="truncate font-mono text-xs text-muted-foreground">
                          {p.addr}
                        </span>
                      </span>
                    ))}
                  </span>
                </Row>
              </>
            ) : (
              <Row label={loading ? 'Reading…' : 'No status yet'}>
                {loading ? <Spinner /> : null}
              </Row>
            )}
          </Group>
        </div>
      )}
    </section>
  )
}

/* one or more mono lines, right-aligned, with a copy button when there is something to copy */
function Mono({ lines, copy }: { lines: string[]; copy?: string | null }) {
  return (
    <span className="flex max-w-[28rem] items-start justify-end gap-1">
      <span className="grid gap-0.5 text-right font-mono text-xs text-muted-foreground">
        {lines.length ? (
          lines.map((l) => (
            <span key={l} className="truncate">
              {l}
            </span>
          ))
        ) : (
          <span>none</span>
        )}
      </span>
      {copy && <CopyButton getText={() => copy} className="-my-1" />}
    </span>
  )
}
