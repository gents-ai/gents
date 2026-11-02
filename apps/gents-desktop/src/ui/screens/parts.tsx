import type { ComponentProps } from "react";
import { cn } from "@gents/ui/lib/utils";
import type { AgentEnvironmentView } from "@source-inc/gents-desktop-client";
import { bashAccess, fileAccess, initials, network } from "./behavior";

export function AgentInitials({
  name,
  className,
  ...props
}: { name: string; className?: string } & Omit<ComponentProps<"span">, "children">) {
  return (
    <span
      {...props}
      className={cn(
        "grid size-7 shrink-0 place-items-center rounded-full border border-border bg-raised text-[11px] font-medium text-foreground",
        className,
      )}
      aria-hidden="true"
    >
      {initials(name)}
    </span>
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
