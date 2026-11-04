import { useEffect, useState, type ReactNode } from "react";
import type {
  DesktopOperationsSnapshot,
  InstalledPack,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { nodeKeyOf, type NodeView } from "../../../hooks/fleetStore";
import type { ShellActions } from "@/../hooks/shellActions";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { Switch } from "@gents/ui/components/switch";
import { Pencil } from "lucide-react";
import { Skeleton } from "@gents/ui/components/skeleton";
import { href, type Route } from "@/lib/router";
import { AvatarRoll } from "../AvatarRoll";
import { isLive } from "@/lib/live";
import { useApp } from "@/app/AppContext";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { useHomeDid, useNodeReach } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { InferenceSummary, OnlineStatus, StateDot, counted } from "../NodeFacts";
import { AgentInitials } from "../parts";
import { agentName } from "../behavior";
import { span, useMinute } from "../time";
import { lastRunFailed } from "./automation";
import { limitNote } from "./ProviderUsage";
import { useProviderUsage } from "@/hooks/useProviders";
import { ProviderLogo } from "../ProviderLogo";
import { agentOf, backendOf } from "@/lib/agents";
import { RunNodeButton } from "./LocalServer";
import { errorText } from "../../../lib/errorText";
import { shortPath } from "@/lib/paths";
import { NO_SESSIONS, useFleet } from "@/hooks/useFleet";

function pulse(
  sessions: readonly SessionSummary[],
  ops: DesktopOperationsSnapshot | null,
) {
  const live = sessions.filter((s) => isLive(s.turnState)).length;
  const tools = ops?.backgroundedTools ?? [];
  const workers = tools.filter((t) => t.startsSession).length;
  /* a plugin job is a workflow, not a background command */
  const workflows = tools.filter((t) => t.job && !t.startsSession).length;
  const jobs = tools.length - workers - workflows;
  const overdue = tools.filter((t) => t.deadlineExpired).length;
  const parts = [
    live ? `${counted(live, "session")} live` : null,
    workers ? `${workers} ${workers === 1 ? "session call" : "session calls"}` : null,
    workflows ? `${workflows} ${workflows === 1 ? "workflow" : "workflows"}` : null,
    jobs ? `${jobs} background ${jobs === 1 ? "job" : "jobs"}` : null,
    overdue ? `${overdue} past ${overdue === 1 ? "its" : "their"} deadline` : null,
  ].filter(Boolean) as string[];
  return {
    text: parts.length ? parts.join(" · ") : "Idle",
    busy: parts.length > 0,
    overdue,
  };
}

/** Keeps `avatar` as the node's, leaving its other settings as saved. */
export async function saveNodeAvatar(
  changeConfig: ShellActions["changeConfig"],
  deployment: NodeView,
  avatar: string,
) {
  const node = deployment.nodeConfig;
  if (!node) throw new Error("The node's settings have not loaded yet.");
  await changeConfig("saveNodeConfig", { document: { ...node, avatar } });
}

/* a card under the node's header: one thing about the node right now, and
   the way to where it is set */
/** A summary card under a page's header; with `to`, the whole card leads
    there. Node, agent and task pages share them. */
export function Summary({
  label,
  to,
  children,
}: {
  label: string;
  to?: Route;
  children: ReactNode;
}) {
  const content = (
    <>
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="flex min-w-0 items-center gap-2 truncate text-sm underline-offset-2 group-hover/summary:underline group-focus-visible/summary:underline">
        {children}
      </span>
    </>
  );
  const look = "group/summary grid min-w-0 gap-1 rounded-xl bg-raised/50 px-3 py-2.5";
  return to ? (
    <a
      href={href(to)}
      className={`${look} transition-colors hover:bg-raised focus-visible:bg-raised focus-visible:outline-none`}
    >
      {content}
    </a>
  ) : (
    <div className={look}>{content}</div>
  );
}

type PacksRead = InstalledPack[] | { failed: string } | undefined;

/** The node's installed packs: undefined while read, and why when the read
    failed. */
function usePacks(): PacksRead {
  const { actions } = useApp();
  const [packs, setPacks] = useState<PacksRead>(undefined);
  useEffect(() => {
    let alive = true;
    /* an adapter without the read, like one that fails it, leaves packs unknown */
    Promise.resolve()
      .then(() => actions.listInstalledPacks())
      .then(
        (report) => alive && setPacks(report.packs),
        (error: unknown) => alive && setPacks({ failed: errorText(error) }),
      );
    return () => {
      alive = false;
    };
  }, [actions]);
  return packs;
}

function PacksValue({ packs }: { packs: PacksRead }) {
  if (packs === undefined) return <Skeleton className="h-4 w-28 bg-foreground/10" />;
  if (!Array.isArray(packs))
    return (
      <span title={packs.failed} className="truncate text-muted-foreground">
        Couldn’t read
      </span>
    );
  if (packs.length === 0)
    return (
      <span className="truncate">
        None installed ·{" "}
        <span className="underline-offset-2 group-hover/summary:underline group-focus-visible/summary:underline">
          Install a pack →
        </span>
      </span>
    );
  const broken = packs.filter((p) => p.error).length;
  const outdated = packs.filter((p) => p.outdated).length;
  return (
    <span className="truncate">
      {packs.length} installed
      {broken > 0 && <span className="text-warning"> · {broken} with a problem</span>}
      {outdated > 0 && ` · ${outdated} to update`}
    </span>
  );
}

/* the agents a card shows marks for before the rest are counted as +n */
const MARKS = 4;

/** The node's agents: a few of their marks, then how many more, and how
    many there are. */
function AgentsValue({ deployment }: { deployment: NodeView }) {
  const agents = deployment.agents;
  if (agents.length === 0) return <span className="truncate">None</span>;
  const more = agents.length - MARKS;
  const mark = "-ml-1.5 size-6 text-[10px] ring-2 ring-background first:ml-0";
  return (
    <>
      <span aria-hidden className="flex shrink-0 items-center">
        {agents.slice(0, MARKS).map((a) => (
          <AgentInitials
            key={a.agentId}
            name={agentName(a.agentId, deployment)}
            className={mark}
          />
        ))}
        {more > 0 && (
          <span
            className={`grid shrink-0 place-items-center rounded-full border border-border bg-raised font-medium text-muted-foreground ${mark}`}
          >
            +{more}
          </span>
        )}
      </span>
      <span className="truncate text-muted-foreground">
        {counted(agents.length, "agent")}
      </span>
    </>
  );
}

/** The node's automations: how many, when the next one runs, and how many
    of those that are on failed last time. */
function AutomationsValue({ deployment }: { deployment: NodeView }) {
  const now = useMinute();
  const tasks = deployment.tasks.length;
  if (tasks === 0) return <span className="truncate">None</span>;
  const on = deployment.triggers.filter((t) => t.config.enabled !== false);
  const failed = on.filter(lastRunFailed).length;
  const next = on
    .flatMap((t) => (t.nextRunAt ? [Date.parse(t.nextRunAt)] : []))
    .filter((at) => at > now)
    .sort((a, b) => a - b)[0];
  return (
    <span className="truncate">
      {counted(tasks, "automation")}
      {next !== undefined && (
        <span title={new Date(next).toLocaleString()}>
          {" "}
          · next in {span(next - now)}
        </span>
      )}
      {failed > 0 && <span className="text-warning"> · {failed} failed</span>}
    </span>
  );
}

/** The node page's header: what the node is called, its state and summary;
    with `edit`, its tags show and its name and tags can be opened to edit;
    its portrait rolls into the settings form's draft. */
export function AgentCard({
  deployment,
  edit,
  avatar,
  onAvatar,
}: {
  deployment: NodeView;
  edit?: {
    open: boolean;
    onToggle: () => void;
    /** opens the fields at the tags */
    onTags: () => void;
    controls: string;
    tags: string[];
    /** switched here, saved with the rest of the form */
    enabled: boolean;
    onEnabled: (enabled: boolean) => void;
  };
  avatar: string | null;
  onAvatar: (avatar: string) => void;
}) {
  const {
    actions: { fetchOperationsSnapshot },
  } = useApp();
  const reach = useNodeReach(deployment);
  const home = useHomeFolder();
  const homeDid = useHomeDid();
  const packs = usePacks();
  const node = deployment.node;
  const name = node.displayName ?? deployment.label;
  const [ops, setOps] = useState<DesktopOperationsSnapshot | null>(null);
  /* background jobs come from a read; until its first answer the card waits */
  const [opsRead, setOpsRead] = useState(false);
  const sessions = useFleet((s) => s.sessionsOf[nodeKeyOf(deployment)] ?? NO_SESSIONS);
  useEffect(() => {
    let alive = true;
    void fetchOperationsSnapshot({ nodeDid: deployment.nodeDid }).then(
      (o) => {
        if (!alive) return;
        setOps(o);
        setOpsRead(true);
      },
      () => {
        if (!alive) return;
        setOps(null);
        setOpsRead(true);
      },
    );
    return () => {
      alive = false;
    };
  }, [fetchOperationsSnapshot, deployment.nodeDid, sessions]);
  const now = pulse(sessions, ops);
  const limit = limitNote(useProviderUsage(deployment.nodeDid).usage, useMinute());
  const online = deployment.dialSucceeded;
  const backends = deployment.inferenceBackends;
  const section = (section: string): Route => ({
    name: "agent",
    nodeDid: deployment.nodeDid,
    section,
  });
  const facts = [
    reach.ceiling,
    reach.root && shortPath(reach.root, { home: reach.local ? home : null, max: 40 }),
  ].filter(Boolean) as string[];
  return (
    <div className="@container mb-8 grid gap-4">
      <div className="flex items-start gap-4">
        <AvatarRoll
          rollId={deployment.nodeDid}
          name={name}
          avatar={avatar}
          disabled={!deployment.nodeConfig}
          onChange={onAvatar}
        />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="truncate font-heading text-lg font-medium text-heading">
              {name}
            </h2>
            {node.enabled === false && <Badge variant="outline">Disabled</Badge>}
            <OnlineStatus online={online} />
            {edit && (
              <Button
                size="xs"
                variant="quiet"
                aria-expanded={edit.open}
                aria-controls={edit.controls}
                onClick={edit.onToggle}
              >
                {edit.open ? (
                  "Done"
                ) : (
                  <>
                    <Pencil /> Edit
                  </>
                )}
              </Button>
            )}
          </div>
          <p className="mt-1 truncate text-xs text-muted-foreground">
            {facts.join(" · ")}
            {/* the host's id in full only on hover; its end tells hosts apart */}
            {deployment.peerId && (
              <>
                {facts.length ? " · " : ""}
                <span title={deployment.peerId} className="font-mono">
                  {deployment.peerId.slice(-8)}
                </span>
              </>
            )}
          </p>
          {edit && edit.tags.length > 0 && (
            <ul aria-label="Tags" className="mt-2 flex flex-wrap gap-1">
              {edit.tags.map((tag) => (
                <li key={tag}>
                  <button
                    type="button"
                    onClick={edit.onTags}
                    className="cursor-pointer rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
                  >
                    {tag}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-3">
          {edit && (
            <label
              className="flex items-center gap-2.5 text-sm"
              title="A disabled node accepts no requests."
            >
              {edit.enabled ? "Enabled" : "Disabled"}
              <Switch
                aria-label="Enabled"
                checked={edit.enabled}
                onCheckedChange={edit.onEnabled}
              />
            </label>
          )}
          {isLocalNode(deployment, homeDid) && <RunNodeButton />}
        </div>
      </div>
      <div className="grid gap-2 @xl:grid-cols-3">
        <Summary label="Activity" to={{ name: "sessions" }}>
          {opsRead ? (
            <>
              <StateDot tone={now.overdue ? "bad" : now.busy ? "busy" : "idle"} />
              <span className="truncate">{now.text}</span>
            </>
          ) : (
            <Skeleton className="h-4 w-24 bg-foreground/10" />
          )}
        </Summary>
        <Summary label="Agents" to={section("agents")}>
          <AgentsValue deployment={deployment} />
        </Summary>
        <Summary label="Automations" to={section("tasks")}>
          <AutomationsValue deployment={deployment} />
        </Summary>
        <Summary label="Inference" to={section("inference")}>
          <InferenceSummary backends={backends} limit={limit} />
        </Summary>
        <Summary label="Packs" to={section("packs")}>
          <PacksValue packs={packs} />
        </Summary>
      </div>
    </div>
  );
}

/** The agent page's cards, as the node page's: what it runs on, what it is
    doing, and what runs with it on its own. */
export function AgentSummaryCards({
  deployment,
  agentId,
}: {
  deployment: NodeView;
  agentId: string;
}) {
  const agent = agentOf(deployment, agentId);
  const sessions = useFleet((s) => s.sessionsOf[nodeKeyOf(deployment)] ?? NO_SESSIONS);
  if (!agent) return null;
  const env = deployment.agentEnvironments.find((e) => e.agentId === agentId);
  const backend = backendOf(deployment, agent);
  const mine = sessions.filter((s) => s.agentId === agentId);
  const live = mine.filter((s) => isLive(s.turnState)).length;
  const tasks = deployment.tasks.filter((t) => t.agentId === agentId).length;
  const section = (section: string, item?: string): Route => ({
    name: "agent",
    nodeDid: deployment.nodeDid,
    section,
    ...(item ? { item } : {}),
  });
  return (
    <div className="@container mb-8">
      <div className="grid gap-2 @xl:grid-cols-3">
        <Summary
          label="Runs on"
          to={backend ? section("inference", backend.backendId) : section("inference")}
        >
          {backend && (
            <ProviderLogo kind={backend.providerKind} endpoint={backend.endpoint} />
          )}
          <span className="truncate">{env?.modelName ?? "No model"}</span>
        </Summary>
        <Summary label="Activity" to={{ name: "sessions" }}>
          <StateDot tone={live ? "busy" : "idle"} />
          <span className="truncate">
            {live
              ? `${counted(live, "session")} live`
              : `Idle · ${counted(mine.length, "session")}`}
          </span>
        </Summary>
        <Summary label="Tasks" to={section("tasks")}>
          <span className="truncate">{tasks ? counted(tasks, "task") : "None"}</span>
        </Summary>
      </div>
    </div>
  );
}
