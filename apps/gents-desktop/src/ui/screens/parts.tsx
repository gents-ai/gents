import type { ComponentProps, CSSProperties } from "react";
import { cn } from "@gents/ui/lib/utils";
import type { AgentEnvironmentView } from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../hooks/fleetStore";
import {
  agentHue,
  agentName,
  bashAccess,
  fileAccess,
  initials,
  isUnknownAgent,
  nameShared,
  network,
} from "./behavior";

/** An agent's initials; with `shared`, when another agent beside it goes by
    the same name, or when it is unknown, tinted by its DID so they differ. */
export function AgentInitials({
  name,
  agentDid,
  shared = false,
  className,
  style,
  ...props
}: {
  name: string;
  agentDid?: string | null;
  shared?: boolean;
  className?: string;
} & Omit<ComponentProps<"span">, "children">) {
  const unknown = isUnknownAgent(name);
  /* unknown agents all draw as "?", so they too take their DIDs' tints */
  const hue = (shared || unknown) && agentDid ? agentHue(agentDid) : null;
  return (
    <span
      {...props}
      style={hue === null ? style : ({ ...style, "--agent-hue": hue } as CSSProperties)}
      className={cn(
        "grid size-7 shrink-0 place-items-center rounded-full border border-border bg-raised text-[11px] font-medium text-foreground",
        hue !== null &&
          "bg-[color-mix(in_oklch,var(--color-raised)_65%,oklch(0.72_0.14_var(--agent-hue)))]",
        className,
      )}
      aria-hidden="true"
    >
      {unknown ? "?" : initials(name)}
    </span>
  );
}

/** An agent's initials as `deployment` knows it, tinted when another agent
    there shares its name. */
export function AgentMark({
  agentDid,
  deployment,
  ...props
}: { agentDid: string | null; deployment: NodeView | null } & Omit<
  ComponentProps<typeof AgentInitials>,
  "name" | "agentDid" | "shared"
>) {
  return (
    <AgentInitials
      {...props}
      name={agentName(agentDid, deployment)}
      agentDid={agentDid}
      shared={nameShared(agentDid, deployment)}
    />
  );
}

export type AccessPart = "files" | "bash" | "network";

/** What an agent may do, in a sentence whose parts stand out; with `onOpen`,
    each part leads to where it is set. */
export function AccessSentence({
  name,
  env,
  onOpen,
}: {
  name: string;
  env:
    | Pick<AgentEnvironmentView, "fileAccess" | "bashAccess" | "networkAccess">
    | undefined;
  onOpen?: (part: AccessPart) => void;
}) {
  const part = (part: AccessPart, children: string) =>
    onOpen ? (
      <button
        type="button"
        onClick={() => onOpen(part)}
        className="cursor-pointer rounded-sm font-medium text-foreground underline-offset-2 outline-none hover:underline focus-visible:underline"
      >
        {children}
      </button>
    ) : (
      <strong className="font-medium text-foreground">{children}</strong>
    );
  return (
    <>
      {name} can{" "}
      {env ? (
        <>
          {part("files", `${fileAccess(env.fileAccess)} files`)} and{" "}
          {part("bash", `${bashAccess(env.bashAccess)} commands`)}
        </>
      ) : (
        "…"
      )}
      , and has access to {part("network", network(env?.networkAccess))}.
    </>
  );
}
