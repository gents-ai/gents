/* Facts about a node or agent, drawn the same wherever they are shown: the
   hover cards' lines and the node and agent pages' summary cards. */
import { cn } from "@gents/ui/lib/utils";
import type { NodeView } from "../../hooks/fleetStore";
import { providerLogo } from "@/lib/provider";
import { ProviderLogo } from "./ProviderLogo";
import { healthy } from "./agent/inferenceKinds";

type Backends = NodeView["inferenceBackends"];

/** "1 task", "3 tasks" */
export const counted = (n: number, one: string, many = `${one}s`) =>
  `${n} ${n === 1 ? one : many}`;

export type Tone = "good" | "busy" | "bad" | "idle";

export function StateDot({ tone }: { tone: Tone }) {
  return (
    <span
      aria-hidden
      className={cn(
        "size-1.5 shrink-0 rounded-full",
        tone === "bad" ? "bg-destructive" : tone === "idle" ? "bg-border" : "bg-brand",
      )}
    />
  );
}

export function OnlineStatus({ online }: { online: boolean }) {
  return (
    <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
      <StateDot tone={online ? "good" : "bad"} />
      {online ? "Online" : "Offline"}
    </span>
  );
}

/* one logo per provider, however many of its accounts the node has; the
   accounts themselves are named on hover */
export function InferenceLogos({ backends }: { backends: Backends }) {
  const providers = new Map<string, Backends[number]>();
  for (const b of backends) {
    const key = providerLogo(b.providerKind, b.endpoint) ?? "other";
    if (!providers.has(key)) providers.set(key, b);
  }
  const shown = [...providers.values()];
  const names = backends.map((b) => b.name ?? b.backendId).join(", ");
  return (
    <span
      role="img"
      aria-label={names}
      title={names}
      className="-my-0.5 inline-flex shrink-0 items-center align-middle"
    >
      {shown.slice(0, 3).map((b, i) => (
        <span
          key={b.backendId}
          className={cn(
            "grid size-5 place-items-center rounded-full bg-muted ring-2 ring-popover",
            i > 0 && "-ml-1.5",
          )}
        >
          <ProviderLogo
            kind={b.providerKind}
            endpoint={b.endpoint}
            className="size-3"
          />
        </span>
      ))}
      {shown.length > 3 && (
        <span className="ml-1 text-muted-foreground tabular-nums">
          +{shown.length - 3}
        </span>
      )}
    </span>
  );
}

export function inferenceState(backends: Backends) {
  if (backends.length === 0) return "None set up";
  const unready = backends.filter((b) => !healthy(b.probeStatus)).length;
  return unready ? `${unready} not ready` : "Ready";
}

/** A node's inference: its providers' logos and whether they are ready. */
export function InferenceSummary({ backends }: { backends: Backends }) {
  return (
    <span className="inline-flex min-w-0 items-center gap-2 align-middle">
      {backends.length > 0 && <InferenceLogos backends={backends} />}
      <span className="truncate">{inferenceState(backends)}</span>
    </span>
  );
}
